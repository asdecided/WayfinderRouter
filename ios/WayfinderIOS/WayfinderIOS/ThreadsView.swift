import SwiftUI

struct ThreadsView: View {
  @Environment(AppModel.self) private var appModel
  @State private var searchText = ""
  @State private var renaming: ConversationThreadSnapshot?
  @State private var proposedTitle = ""

  var openSidebar: (() -> Void)?

  private var results: [ConversationThreadSnapshot] {
    appModel.threads(matching: searchText)
  }

  var body: some View {
    Group {
      if appModel.isRestoringConversations, appModel.threads.isEmpty {
        // A designed loading state rather than a bare spinner.
        ThreadListSkeleton()
      } else if appModel.threads.isEmpty {
        ContentUnavailableView(
          "No chats yet",
          systemImage: "bubble.left.and.bubble.right",
          description: Text("Start a chat and it will appear here.")
        )
      } else if results.isEmpty {
        ContentUnavailableView.search(text: searchText)
      } else {
        list
      }
    }
    .navigationTitle("Chats")
    .searchable(text: $searchText, prompt: "Search chats")
    .toolbar {
      if let openSidebar {
        SidebarToolbarButton(action: openSidebar)
      }
    }
    .sheet(item: $renaming) { thread in
      RenameThreadSheet(
        title: $proposedTitle,
        save: { newTitle in
          Task { await appModel.renameThread(id: thread.id, to: newTitle) }
        }
      )
      .presentationDetents([.height(200)])
    }
    .task {
      await appModel.restoreConversations()
    }
  }

  private var list: some View {
    List {
      ForEach(results) { thread in
        Button {
          Task {
            await appModel.selectThread(id: thread.id)
          }
        } label: {
          ThreadRow(thread: thread)
        }
        .buttonStyle(.plain)
        .swipeActions(edge: .leading, allowsFullSwipe: true) {
          Button {
            Task {
              await appModel.setPinned(!thread.isPinned, threadID: thread.id)
            }
          } label: {
            Label(
              thread.isPinned ? "Unpin" : "Pin",
              systemImage: thread.isPinned ? "pin.slash" : "pin"
            )
          }
          .tint(WayfinderTheme.accent)
        }
        .swipeActions(edge: .trailing) {
          Button(role: .destructive) {
            Task { await appModel.deleteThread(id: thread.id) }
          } label: {
            Label("Delete", systemImage: "trash")
          }

          Button {
            beginRenaming(thread)
          } label: {
            Label("Rename", systemImage: "pencil")
          }
        }
        .contextMenu {
          Button {
            beginRenaming(thread)
          } label: {
            Label("Rename", systemImage: "pencil")
          }
          Button {
            Task {
              await appModel.setPinned(!thread.isPinned, threadID: thread.id)
            }
          } label: {
            Label(
              thread.isPinned ? "Unpin" : "Pin",
              systemImage: thread.isPinned ? "pin.slash" : "pin"
            )
          }
          Button(role: .destructive) {
            Task { await appModel.deleteThread(id: thread.id) }
          } label: {
            Label("Delete", systemImage: "trash")
          }
        }
        .accessibilityAction(named: "Rename") { beginRenaming(thread) }
        .accessibilityAction(named: thread.isPinned ? "Unpin" : "Pin") {
          Task {
            await appModel.setPinned(!thread.isPinned, threadID: thread.id)
          }
        }
        .accessibilityAction(named: "Delete") {
          Task { await appModel.deleteThread(id: thread.id) }
        }
      }
    }
  }

  private func beginRenaming(_ thread: ConversationThreadSnapshot) {
    proposedTitle = thread.title
    renaming = thread
  }
}

private struct ThreadRow: View {
  let thread: ConversationThreadSnapshot

  var body: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.hairline) {
      HStack(spacing: WayfinderSpacing.hairline + 2) {
        if thread.isPinned {
          Image(systemName: "pin.fill")
            .font(.caption2)
            .foregroundStyle(WayfinderTheme.accent)
            .accessibilityHidden(true)
        }
        Text(thread.title)
          .font(.body.weight(.medium))
          .foregroundStyle(.primary)
          .lineLimit(2)
      }

      HStack {
        Text("\(thread.messages.count) \(thread.messages.count == 1 ? "turn" : "turns")")
        Spacer()
        Text(thread.updatedAt.relativeDescription)
      }
      .font(.caption)
      .foregroundStyle(.secondary)
    }
    .padding(.vertical, WayfinderSpacing.hairline)
    .frame(minHeight: WayfinderMetrics.minimumHitTarget)
    .contentShape(Rectangle())
    .accessibilityElement(children: .combine)
    .accessibilityLabel(thread.isPinned ? "\(thread.title), pinned" : thread.title)
  }
}

private struct RenameThreadSheet: View {
  @Environment(\.dismiss) private var dismiss
  @FocusState private var fieldFocused: Bool

  @Binding var title: String
  let save: (String) -> Void

  var body: some View {
    NavigationStack {
      Form {
        TextField("Conversation name", text: $title)
          .focused($fieldFocused)
          .submitLabel(.done)
          .onSubmit(commit)
      }
      .navigationTitle("Rename")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("Cancel") { dismiss() }
        }
        ToolbarItem(placement: .confirmationAction) {
          Button("Save", action: commit)
            .disabled(
              title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            )
        }
      }
      .onAppear { fieldFocused = true }
    }
  }

  private func commit() {
    let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { return }
    save(trimmed)
    dismiss()
  }
}

/// Progressive reveal while threads load, instead of a bare spinner.
private struct ThreadListSkeleton: View {
  var body: some View {
    List {
      ForEach(0..<5, id: \.self) { index in
        VStack(alignment: .leading, spacing: WayfinderSpacing.xSmall) {
          RoundedRectangle(cornerRadius: 4)
            .fill(.quaternary)
            .frame(width: index.isMultiple(of: 2) ? 220 : 160, height: 14)
          RoundedRectangle(cornerRadius: 4)
            .fill(.quaternary)
            .frame(width: 96, height: 10)
        }
        .padding(.vertical, WayfinderSpacing.hairline)
      }
    }
    .disabled(true)
    .accessibilityElement()
    .accessibilityLabel("Restoring conversations")
    .accessibilityAddTraits(.updatesFrequently)
  }
}
