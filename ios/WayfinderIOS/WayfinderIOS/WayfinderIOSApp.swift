import SwiftUI

@main
struct WayfinderIOSApp: App {
  @State private var appModel: AppModel

  init() {
    let credentialStore = KeychainCredentialStore()
    let provider = OpenAICompatibleProvider(
      configurations: OpenAICompatibleConfiguration.supported,
      credentialStore: credentialStore
    )
    let providerModelCatalog = OpenAICompatibleModelCatalog(
      configurations: OpenAICompatibleConfiguration.supported,
      credentialStore: credentialStore
    )

    do {
      let container = try SwiftDataConversationStore.makeContainer()
      _appModel = State(
        initialValue: AppModel(
          conversationStore: SwiftDataConversationStore(
            modelContainer: container
          ),
          credentialStore: credentialStore,
          providerExecutor: provider,
          providerModelCatalog: providerModelCatalog,
          destinations: .liveDirectProviders
        )
      )
    } catch {
      _appModel = State(
        initialValue: AppModel(
          conversationStore: InMemoryConversationStore(),
          credentialStore: credentialStore,
          providerExecutor: provider,
          providerModelCatalog: providerModelCatalog,
          destinations: .liveDirectProviders,
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
