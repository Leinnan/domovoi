import Foundation
@preconcurrency import Shared

struct TunnelError: Error {
    let message: String
}

/// `TunnelFfi.status()` JSON.
struct TunnelStatus: Decodable {
    let connected: Bool
    let path: String?
    let rttMs: Int?
    let error: String?
    /// False once the loopback port is gone (iOS reclaims a suspended app's
    /// listening socket); absent from older cores, so it defaults to true.
    let listening: Bool?
}

/// Runs the iroh bridge's blocking calls on one serial queue, so a start,
/// a status read and a stop never overlap.
final class TunnelWorker: @unchecked Sendable {
    private let ffi = TunnelFfi()
    private let queue = DispatchQueue(label: "com.crayen.domovoi.tunnel", qos: .userInitiated)
    /// Self-hosted relays, comma separated (`DOMOVOI_RELAYS`); empty uses n0's.
    private let relays = ProcessInfo.processInfo.environment["DOMOVOI_RELAYS"] ?? ""

    func start(key: String, ticket: String, done: @escaping @Sendable (Result<UInt16, TunnelError>) -> Void) {
        queue.async { [ffi, relays] in
            do {
                done(.success(try ffi.start(secretKey: key, ticket: ticket, relays: relays)))
            } catch let error as FfiError {
                done(.failure(TunnelError(message: error.message)))
            } catch {
                done(.failure(TunnelError(message: "\(error)")))
            }
        }
    }

    func status() async -> TunnelStatus? {
        await withCheckedContinuation { continuation in
            queue.async { [ffi] in
                let json = ffi.status()
                continuation.resume(returning: try? JSONDecoder().decode(TunnelStatus.self, from: Data(json.utf8)))
            }
        }
    }

    func networkChanged() {
        queue.async { [ffi] in ffi.networkChange() }
    }

    func stop() {
        queue.async { [ffi] in ffi.stop() }
    }
}
