// Http: `request(req: HttpRequest) -> Result<HttpResponse, HttpError>` (docs/SPEC.md section 8),
// served by URLSession.

import Foundation

/// `Http` on `URLSession`.
///
/// The default session is ephemeral (no cookies, no disk cache): the core has its own query
/// cache and decides what to persist. Pass a configured `URLSession` for anything else (a
/// custom trust policy, a proxy). A background session is not supported: it takes no task
/// delegate, and every request on it fails with `Network` before a task exists (instead of the
/// Objective-C exception URLSession would raise, which aborts the app).
///
/// Failures map to `HttpError`: an unparsable or non-HTTP(S) URL is `InvalidUrl`, an expired
/// timeout is `Timeout`, a cancelled call is `Cancelled`, and any other transport failure is
/// `Network` with the system's description. A response with an error status is not a failure:
/// the core gets it as an `HttpResponse`.
public final class HttpAdapter: UndraAdapter, @unchecked Sendable {
    private let session: URLSession

    /// Creates the adapter over `session`.
    public init(session: URLSession = HttpAdapter.makeDefaultSession()) {
        self.session = session
    }

    /// The session used when none is passed: ephemeral, no waiting for connectivity.
    public static func makeDefaultSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        return URLSession(configuration: configuration)
    }

    public var portId: UInt32 {
        return StandardPorts.Http.portId
    }

    public func makePortImpl(core: UndraCore) -> PortImpl? {
        let session = self.session
        return .async([
            StandardPorts.Http.request: { args in
                var reader = UndraReader(args)
                let request = try HttpRequest.undraDecode(&reader)
                try reader.finish()
                return try await HttpAdapter.perform(request, on: session)
            },
        ])
    }

    /// Runs `request` and returns the encoded `HttpResponse`, or throws `UndraPortError` carrying
    /// the encoded `HttpError`.
    static func perform(_ request: HttpRequest, on session: URLSession) async throws -> [UInt8] {
        guard let url = URL(string: request.url),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = url.host,
              !host.isEmpty
        else {
            throw HttpAdapter.portError(.invalidUrl(request.url))
        }
        // A background session (the only kind with an identifier) takes no task delegate and no completion handler, and
        // `data(for:)` needs one: URLSession raises an Objective-C exception that would abort the app, so the request fails
        // before a task exists.
        guard session.configuration.identifier == nil else {
            throw HttpAdapter.portError(.network(
                "a background URLSession cannot carry the core's requests (it takes no task delegate): "
                    + "give HttpAdapter a default or ephemeral session"
            ))
        }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = request.method.name
        for header in request.headers {
            urlRequest.addValue(header.value, forHTTPHeaderField: header.name)
        }
        if let body = request.body {
            urlRequest.httpBody = Data(body)
        }
        if let timeoutMs = request.timeoutMs {
            urlRequest.timeoutInterval = Swift.max(Double(timeoutMs) / 1000.0, 0.001)
        }
        do {
            let (data, response) = try await session.data(for: urlRequest)
            guard let http = response as? HTTPURLResponse else {
                throw HttpAdapter.portError(.network("the server did not answer with an HTTP response"))
            }
            let reply = HttpResponse(
                status: UInt16(truncatingIfNeeded: http.statusCode),
                headers: HttpAdapter.headers(of: http),
                body: [UInt8](data)
            )
            return reply.undraEncoded()
        } catch let error as UndraPortError {
            throw error
        } catch is CancellationError {
            throw HttpAdapter.portError(.cancelled)
        } catch let error as URLError {
            throw HttpAdapter.portError(HttpAdapter.map(error, url: request.url))
        } catch {
            throw HttpAdapter.portError(.network(String(describing: error)))
        }
    }

    /// The response headers, sorted by name (the dictionary URLSession returns is unordered).
    static func headers(of response: HTTPURLResponse) -> [Header] {
        var result: [Header] = []
        for (key, value) in response.allHeaderFields {
            if let name = key as? String, let text = value as? String {
                result.append(Header(name: name, value: text))
            }
        }
        result.sort { (left: Header, right: Header) -> Bool in
            if left.name != right.name {
                return left.name < right.name
            }
            return left.value < right.value
        }
        return result
    }

    static func map(_ error: URLError, url: String) -> HttpError {
        switch error.code {
        case .timedOut:
            return .timeout
        case .cancelled:
            return .cancelled
        case .badURL, .unsupportedURL:
            return .invalidUrl(url)
        default:
            return .network(error.localizedDescription)
        }
    }

    private static func portError(_ error: HttpError) -> UndraPortError {
        return UndraPortError(body: error.undraEncoded())
    }
}
