import SwiftUI

@main
struct WayfinderIOSApp: App {
  @State private var appModel: AppModel

  init() {
    do {
      let container = try SwiftDataConversationStore.makeContainer()
      _appModel = State(
        initialValue: AppModel(
          conversationStore: SwiftDataConversationStore(
            modelContainer: container
          )
        )
      )
    } catch {
      _appModel = State(
        initialValue: AppModel(
          conversationStore: InMemoryConversationStore(),
          initialPersistenceNotice:
            "Saved conversations are unavailable. New chats will remain in memory until Wayfinder restarts."
        )
      )
    }
  }

  var body: some Scene {
    WindowGroup {
      RootView()
        .environment(appModel)
        .tint(WayfinderTheme.accent)
        .task {
          await appModel.restoreConversations()
          await appModel.restoreCredentialStatuses()
        }
    }
  }
}
