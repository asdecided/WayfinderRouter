import Foundation

struct DiscoveredProviderModel: Equatable, Sendable {
  let providerID: String
  let modelID: String
  let displayName: String
  let contextWindow: UInt64?
}

enum ProviderModelInventoryState: Equatable, Sendable {
  case loaded([DiscoveredProviderModel])
  case notConfigured
  case failed
}

struct ProviderModelInventoryResult: Equatable, Sendable {
  let providerID: String
  let state: ProviderModelInventoryState
}

protocol ProviderModelCatalog: Sendable {
  func refresh() async -> [ProviderModelInventoryResult]
}

struct EmptyProviderModelCatalog: ProviderModelCatalog {
  func refresh() async -> [ProviderModelInventoryResult] {
    []
  }
}

struct HTTPDataResponse: Sendable {
  let statusCode: Int
  let body: Data
}

protocol HTTPDataClient: Sendable {
  func data(_ request: URLRequest) async throws -> HTTPDataResponse
}

actor URLSessionHTTPDataClient: HTTPDataClient {
  private let session: URLSession

  init(session: URLSession = URLSession(configuration: .ephemeral)) {
    self.session = session
  }

  func data(_ request: URLRequest) async throws -> HTTPDataResponse {
    let (body, response) = try await session.data(for: request)
    guard let httpResponse = response as? HTTPURLResponse else {
      throw ProviderModelCatalogError.invalidResponse
    }
    return HTTPDataResponse(
      statusCode: httpResponse.statusCode,
      body: body
    )
  }
}

enum ProviderModelCatalogError: Error, Equatable, Sendable {
  case invalidResponse
  case responseTooLarge
  case tooManyModels
  case rejected
}

actor OpenAICompatibleModelCatalog: ProviderModelCatalog {
  static let maximumResponseBytes = 4_194_304
  static let maximumModelsPerProvider = 1_000
  static let maximumIdentifierCharacters = 256

  private let configurations: [OpenAICompatibleConfiguration]
  private let credentialStore: any CredentialStore
  private let httpClient: any HTTPDataClient

  init(
    configurations: [OpenAICompatibleConfiguration],
    credentialStore: any CredentialStore,
    httpClient: any HTTPDataClient = URLSessionHTTPDataClient()
  ) {
    var seenProviderIDs: Set<String> = []
    self.configurations = configurations.filter { configuration in
      guard
        configuration.modelsEndpoint != nil,
        !seenProviderIDs.contains(configuration.providerID)
      else {
        return false
      }
      seenProviderIDs.insert(configuration.providerID)
      return true
    }
    self.credentialStore = credentialStore
    self.httpClient = httpClient
  }

  func refresh() async -> [ProviderModelInventoryResult] {
    var results: [ProviderModelInventoryResult] = []

    for configuration in configurations {
      do {
        guard
          let apiKey = try await credentialStore.readSecret(
            for: configuration.credentialID
          ),
          !apiKey.isEmpty
        else {
          results.append(
            ProviderModelInventoryResult(
              providerID: configuration.providerID,
              state: .notConfigured
            )
          )
          continue
        }

        let models = try await loadModels(
          configuration: configuration,
          apiKey: apiKey
        )
        results.append(
          ProviderModelInventoryResult(
            providerID: configuration.providerID,
            state: .loaded(models)
          )
        )
      } catch {
        results.append(
          ProviderModelInventoryResult(
            providerID: configuration.providerID,
            state: .failed
          )
        )
      }
    }

    return results
  }

  private func loadModels(
    configuration: OpenAICompatibleConfiguration,
    apiKey: String
  ) async throws -> [DiscoveredProviderModel] {
    guard let modelsEndpoint = configuration.modelsEndpoint else {
      return []
    }

    var request = URLRequest(url: modelsEndpoint)
    request.httpMethod = "GET"
    request.timeoutInterval = 30
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    request.setValue(
      "Bearer \(apiKey)",
      forHTTPHeaderField: "Authorization"
    )

    let response = try await httpClient.data(request)
    guard response.statusCode == 200 else {
      throw ProviderModelCatalogError.rejected
    }
    guard response.body.count <= Self.maximumResponseBytes else {
      throw ProviderModelCatalogError.responseTooLarge
    }

    struct ModelList: Decodable {
      struct Model: Decodable {
        let id: String
        let name: String?
        let contextLength: UInt64?

        enum CodingKeys: String, CodingKey {
          case id
          case name
          case contextLength = "context_length"
        }
      }

      let data: [Model]
    }

    let payload: ModelList
    do {
      payload = try JSONDecoder().decode(ModelList.self, from: response.body)
    } catch {
      throw ProviderModelCatalogError.invalidResponse
    }
    guard payload.data.count <= Self.maximumModelsPerProvider else {
      throw ProviderModelCatalogError.tooManyModels
    }

    var modelsByID: [String: DiscoveredProviderModel] = [:]
    for model in payload.data {
      let modelID = model.id.trimmingCharacters(
        in: .whitespacesAndNewlines
      )
      guard
        !modelID.isEmpty,
        modelID.count <= Self.maximumIdentifierCharacters
      else {
        continue
      }

      let candidateName = model.name?.trimmingCharacters(
        in: .whitespacesAndNewlines
      )
      let displayName =
        if let candidateName,
          !candidateName.isEmpty,
          candidateName.count <= Self.maximumIdentifierCharacters
        {
          candidateName
        } else {
          Self.displayName(for: modelID)
        }

      modelsByID[modelID] = DiscoveredProviderModel(
        providerID: configuration.providerID,
        modelID: modelID,
        displayName: displayName,
        contextWindow: model.contextLength
      )
    }

    return modelsByID.values.sorted {
      if $0.displayName == $1.displayName {
        return $0.modelID < $1.modelID
      }
      return $0.displayName < $1.displayName
    }
  }

  private static func displayName(for modelID: String) -> String {
    modelID
      .split(separator: "/")
      .last
      .map(String.init)?
      .replacingOccurrences(of: "-", with: " ")
      .capitalized ?? modelID
  }
}
