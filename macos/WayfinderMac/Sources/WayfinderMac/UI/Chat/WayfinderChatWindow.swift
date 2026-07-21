import SwiftUI

public struct WayfinderChatWindow: View {
    @EnvironmentObject private var appState: AppState
    @State private var selectedTurnID: UUID?
    @State private var searchText = ""
    @State private var columnVisibility: NavigationSplitViewVisibility = .all
    @State private var followsLatestTurn = true
    @State private var searchFocusRequest = 0

    public init() {}

    public var body: some View {
        let turns = ChatTurn.make(from: appState.chatMessages)
        NavigationSplitView(columnVisibility: $columnVisibility) {
            ChatSidebarView(
                conversations: appState.chatConversations,
                activeConversationID: appState.activeChatConversationID,
                searchText: $searchText,
                searchFocusRequest: searchFocusRequest,
                isSending: appState.isSendingMessage,
                onNewChat: appState.startNewChat,
                onSelectConversation: { id in
                    appState.selectChatConversation(id)
                    selectedTurnID = nil
                    followsLatestTurn = true
                }
            )
            .navigationSplitViewColumnWidth(
                min: ChatWorkspaceChrome.sidebarMinimumWidth,
                ideal: ChatWorkspaceChrome.sidebarWidth,
                max: ChatWorkspaceChrome.sidebarMaximumWidth
            )
        } detail: {
            VStack(spacing: 0) {
                ChatToolbar(
                    title: chatTitle(in: turns),
                    showsSidebar: showsSidebar,
                    onToggleSidebar: toggleSidebar,
                    onFocusSearch: focusSearch,
                    canRetry: appState.canRetryChat,
                    canClear: appState.canClearChat,
                    onRetry: retryLastTurn,
                    onClear: appState.startNewChat
                )
                Divider()
                ChatConversationView(
                    turns: turns,
                    selectedTurnID: $selectedTurnID,
                    canRetry: appState.canRetryChat,
                    onRetry: retryLastTurn
                )
                ChatComposerView(
                    draft: $appState.chatDraft,
                    destination: $appState.chatDestination,
                    destinations: appState.chatDestinations,
                    isSending: appState.isSendingMessage,
                    canSend: appState.canSendMessage,
                    onSend: {
                        followsLatestTurn = true
                        appState.sendChatDraft()
                    },
                    onStop: appState.stopChatResponse
                )
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .navigationSplitViewStyle(.prominentDetail)
        .frame(
            minWidth: ChatWorkspaceChrome.minimumWindowWidth,
            minHeight: ChatWorkspaceChrome.minimumWindowHeight
        )
        .background(ChatWorkspaceChrome.canvas)
        .onAppear {
            selectedTurnID = selectedTurnID ?? turns.last?.id
        }
        .onChange(of: turns.map(\.id)) {
            if turns.isEmpty {
                resetWorkspace()
            } else {
                selectedTurnID = ChatWorkspaceSelectionPolicy.resolvedTurnID(
                    current: selectedTurnID,
                    followsLatest: followsLatestTurn,
                    turnIDs: turns.map(\.id)
                )
            }
        }
        .onChange(of: selectedTurnID) {
            guard let selectedTurnID else {
                followsLatestTurn = true
                return
            }
            followsLatestTurn = selectedTurnID == turns.last?.id
        }
    }

    private var showsSidebar: Bool {
        columnVisibility != .detailOnly
    }

    private func toggleSidebar() {
        if showsSidebar {
            columnVisibility = .detailOnly
        } else {
            columnVisibility = .all
        }
    }

    private func focusSearch() {
        columnVisibility = .all
        searchFocusRequest += 1
    }

    private func resetWorkspace() {
        selectedTurnID = nil
        searchText = ""
        followsLatestTurn = true
    }

    private func retryLastTurn() {
        followsLatestTurn = true
        appState.retryLastChatTurn()
    }

    private func chatTitle(in turns: [ChatTurn]) -> String {
        guard let firstPrompt = turns.first?.prompt.text,
              !firstPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return "New chat"
        }
        return firstPrompt
    }

}

private struct ChatToolbar: View {
    let title: String
    let showsSidebar: Bool
    let onToggleSidebar: () -> Void
    let onFocusSearch: () -> Void
    let canRetry: Bool
    let canClear: Bool
    let onRetry: () -> Void
    let onClear: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Button(action: onToggleSidebar) {
                Image(systemName: "sidebar.left")
            }
            .accessibilityLabel(showsSidebar ? "Hide conversation navigator" : "Show conversation navigator")
            .help(showsSidebar ? "Hide turn navigator" : "Show turn navigator")
            .keyboardShortcut("s", modifiers: [.command, .control])

            Button(action: onFocusSearch) {
                Image(systemName: "magnifyingglass")
            }
            .accessibilityLabel("Search conversation")
            .keyboardShortcut("f", modifiers: .command)
            .help("Search conversation (Command-F)")

            Text(title)
                .font(.headline.weight(.semibold))
                .lineLimit(1)
            Spacer()
            if canRetry {
                Button(action: onRetry) {
                    Image(systemName: "arrow.clockwise")
                }
                .accessibilityLabel("Retry last response")
                .keyboardShortcut("r", modifiers: .command)
                .help("Retry the last response")
            }
            Button(action: onClear) {
                Image(systemName: "square.and.pencil")
            }
            .disabled(!canClear)
            .accessibilityLabel("New chat")
            .keyboardShortcut("n", modifiers: .command)
            .help("Start a new in-memory chat (Command-N)")
        }
        .buttonStyle(.borderless)
        .controlSize(.small)
        .padding(.horizontal, 18)
        .frame(height: 48)
        .background(ChatWorkspaceChrome.canvas)
    }
}
