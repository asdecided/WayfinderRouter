import SwiftUI

public struct ChatSidebarView: View {
    let conversations: [ChatConversation]
    let activeConversationID: UUID
    @Binding var searchText: String
    let searchFocusRequest: Int
    let isSending: Bool
    let onNewChat: () -> Void
    let onSelectConversation: (UUID) -> Void

    public init(
        conversations: [ChatConversation],
        activeConversationID: UUID,
        searchText: Binding<String>,
        searchFocusRequest: Int = 0,
        isSending: Bool = false,
        onNewChat: @escaping () -> Void = {},
        onSelectConversation: @escaping (UUID) -> Void = { _ in }
    ) {
        self.conversations = conversations
        self.activeConversationID = activeConversationID
        self._searchText = searchText
        self.searchFocusRequest = searchFocusRequest
        self.isSending = isSending
        self.onNewChat = onNewChat
        self.onSelectConversation = onSelectConversation
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SidebarHeader()

            SearchField(text: $searchText, focusRequest: searchFocusRequest)
                .padding(.horizontal, 12)
                .padding(.bottom, 10)

            Button(action: onNewChat) {
                Label("New chat", systemImage: "square.and.pencil")
                    .font(.callout.weight(.medium))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(isSending)
            .accessibilityHint("Starts a new conversation and keeps existing chats in history.")
            .padding(.horizontal, 14)
            .padding(.vertical, 9)

            Text("Chats")
                .font(.caption2.weight(.semibold))
                .textCase(.uppercase)
                .tracking(0.7)
                .foregroundStyle(ChatWorkspaceChrome.tertiaryText)
                .padding(.horizontal, 14)
                .padding(.bottom, 8)

            if visibleConversations.isEmpty {
                SidebarEmptyState(hasHistory: !conversations.isEmpty)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                    .padding(.top, 24)
            } else {
                List(selection: conversationSelection) {
                    ForEach(visibleConversations) { conversation in
                        SidebarConversationRow(
                            conversation: conversation,
                            isSelected: conversation.id == activeConversationID
                        )
                        .tag(conversation.id)
                    }
                }
                .listStyle(.sidebar)
                .scrollContentBackground(.hidden)
            }

            SidebarStatusFooter(conversationCount: conversations.count)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(ChatWorkspaceChrome.sidebar)
    }

    private var visibleConversations: [ChatConversation] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return conversations }
        return conversations.filter { conversation in
            conversation.title.localizedCaseInsensitiveContains(query)
                || conversation.messages.contains {
                    $0.text.localizedCaseInsensitiveContains(query)
                }
        }
    }

    private var conversationSelection: Binding<UUID?> {
        Binding(
            get: { activeConversationID },
            set: { id in
                guard let id else { return }
                onSelectConversation(id)
            }
        )
    }
}

private struct SidebarHeader: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text("Wayfinder")
                    .font(.headline.weight(.semibold))
                Spacer()
                Image(systemName: "point.topleft.down.curvedto.point.bottomright.up")
                    .foregroundStyle(WayfinderTheme.local)
                    .font(.system(size: 14, weight: .semibold))
            }
            Text("Conversation history")
                .font(.caption)
                .foregroundStyle(ChatWorkspaceChrome.secondaryText)
        }
        .padding(.horizontal, 14)
        .padding(.top, 15)
        .padding(.bottom, 14)
    }
}

private struct SearchField: View {
    @Binding var text: String
    let focusRequest: Int
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(ChatWorkspaceChrome.tertiaryText)
                .accessibilityHidden(true)
            TextField("Search chats", text: $text)
                .textFieldStyle(.plain)
                .font(.caption)
                .focused($focused)
                .accessibilityLabel("Search conversations")
            if !text.isEmpty {
                Button {
                    text = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(ChatWorkspaceChrome.tertiaryText)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear search")
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(ChatWorkspaceChrome.mutedFill, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .stroke(ChatWorkspaceChrome.border, lineWidth: 1)
        )
        .onChange(of: focusRequest) {
            focused = true
        }
    }
}

private struct SidebarStatusFooter: View {
    let conversationCount: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Divider().overlay(ChatWorkspaceChrome.border)
            HStack {
                Label("On this Mac", systemImage: "internaldrive")
                    .font(.caption2)
                Spacer()
                Text(conversationCount == 1 ? "1 chat" : "\(conversationCount) chats")
                    .font(.caption2.monospacedDigit())
            }
        }
        .foregroundStyle(ChatWorkspaceChrome.secondaryText)
        .padding(18)
    }
}

private struct SidebarConversationRow: View {
    let conversation: ChatConversation
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(conversation.title)
                .lineLimit(1)
                .foregroundStyle(.primary)
            Text(subtitle)
                .font(.caption2)
                .foregroundStyle(ChatWorkspaceChrome.secondaryText)
                .lineLimit(1)
        }
        .font(.caption)
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(conversation.title), \(subtitle)")
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var subtitle: String {
        let date = conversation.updatedAt.formatted(date: .abbreviated, time: .shortened)
        let turns = conversation.turnCount == 1 ? "1 turn" : "\(conversation.turnCount) turns"
        return "\(turns) · \(date)"
    }
}

private struct SidebarEmptyState: View {
    let hasHistory: Bool

    var body: some View {
        VStack(spacing: 6) {
            if hasHistory {
                Image(systemName: "magnifyingglass")
                    .font(.callout)
                    .foregroundStyle(ChatWorkspaceChrome.tertiaryText)
            }
            Text(hasHistory ? "No chats found" : "No chats yet")
                .font(.caption.weight(.medium))
            if hasHistory {
                Text("Try another search.")
                    .font(.caption2)
                    .foregroundStyle(ChatWorkspaceChrome.tertiaryText)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 18)
        .foregroundStyle(ChatWorkspaceChrome.secondaryText)
    }
}
