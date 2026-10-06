import Foundation
import Network
import XCTest
import os
@testable import UndraRuntime

// MARK: - Helpers

struct TestFailure: Error, CustomStringConvertible {
    let description: String
}

/// Calls a method of a `PortImpl` the way the runtime does.
enum PortCaller {
    static func callSync(_ impl: PortImpl?, _ method: UInt32, _ args: [UInt8]) throws -> [UInt8] {
        guard case .sync(let table)? = impl, let function = table[method] else {
            throw TestFailure(description: "no synchronous method \(method)")
        }
        return try function(args)
    }

    static func callAsync(_ impl: PortImpl?, _ method: UInt32, _ args: [UInt8]) async throws -> [UInt8] {
        guard case .async(let table)? = impl, let function = table[method] else {
            throw TestFailure(description: "no asynchronous method \(method)")
        }
        return try await function(args)
    }
}

func encodeArgs(_ build: (inout UndraWriter) -> Void) -> [UInt8] {
    var writer = UndraWriter()
    build(&writer)
    return writer.finish()
}

func keyArgs(_ key: String) -> [UInt8] {
    return encodeArgs { (writer: inout UndraWriter) -> Void in
        writer.writeString(key)
    }
}

/// An in-memory `KeyValueBackend`.
final class MemoryBackend: KeyValueBackend, @unchecked Sendable {
    private let store = Guarded<[String: [UInt8]]>([:])

    func get(_ key: String) -> [UInt8]? {
        return store.withLock { (values: inout [String: [UInt8]]) -> [UInt8]? in
            return values[key]
        }
    }

    func set(_ key: String, _ value: [UInt8]) {
        store.withLock { (values: inout [String: [UInt8]]) -> Void in
            values[key] = value
        }
    }

    func delete(_ key: String) {
        store.withLock { (values: inout [String: [UInt8]]) -> Void in
            values[key] = nil
        }
    }

    func list(prefix: String) -> [String] {
        return store.withLock { (values: inout [String: [UInt8]]) -> [String] in
            return values.keys.filter { $0.hasPrefix(prefix) }.sorted()
        }
    }
}

/// A backend whose every operation fails with `StorageError.io("broken")`.
struct BrokenBackend: KeyValueBackend {
    func get(_ key: String) throws(StorageError) -> [UInt8]? {
        throw .io("broken")
    }

    func set(_ key: String, _ value: [UInt8]) throws(StorageError) {
        throw .io("broken")
    }

    func delete(_ key: String) throws(StorageError) {
        throw .io("broken")
    }

    func list(prefix: String) throws(StorageError) -> [String] {
        throw .io("broken")
    }
}

// MARK: - The adapter set

@MainActor
final class AdapterSetTests: XCTestCase {
    func testThePlatformDefaultsCoverEveryStandardPort() {
        let ids = Adapters.platformDefault.all.map { $0.portId }
        XCTAssertEqual(Set(ids).count, ids.count, "one adapter per port")
        let expected: Set<UInt32> = [
            StandardPorts.Http.portId, StandardPorts.Kv.portId, StandardPorts.SecureStore.portId,
            StandardPorts.Fs.portId, StandardPorts.Clock.portId, StandardPorts.Rng.portId,
            StandardPorts.Log.portId, StandardPorts.Timer.portId, StandardPorts.Connectivity.portId,
            StandardPorts.Lifecycle.portId, StandardPorts.Diagnostics.portId,
            // The opt-in ports (ADR-047, ADR-048), for the cores that enable them.
            StandardPorts.WebSocket.portId, StandardPorts.Sse.portId, StandardPorts.Db.portId,
        ]
        XCTAssertEqual(Set(ids), expected)
    }

    func testReplacingAndRemovingAdaptersByPort() {
        let base = Adapters.platformDefault
        let count = base.all.count
        let replaced = base.replacing(portId: StandardPorts.Http.portId, with: .async([:]))
        XCTAssertEqual(replaced.all.count, count)
        XCTAssertTrue(replaced.all.contains { $0 is PortImplAdapter })
        XCTAssertFalse(replaced.all.contains { $0 is HttpAdapter })
        let removed = base.removing(portId: StandardPorts.Http.portId)
        XCTAssertEqual(removed.all.count, count - 1)
        XCTAssertEqual(Adapters.none.all.count, 0)
        let appended = Adapters.none.replacing(ClockAdapter()).replacing(RngAdapter())
        XCTAssertEqual(appended.all.map { $0.portId }, [StandardPorts.Clock.portId, StandardPorts.Rng.portId])
    }

    func testALaterAdapterForTheSamePortWinsAtConstruction() throws {
        let core = try makeCore(FakeTransport())
        let adapters = Adapters([PortImplAdapter(portId: 5, impl: .sync([:])), PortImplAdapter(portId: 5, impl: .async([:]))])
        XCTAssertEqual(adapters.all.count, 1)
        let adapter = try XCTUnwrap(adapters.all.first as? PortImplAdapter)
        guard case .async? = adapter.makePortImpl(core: core) else {
            return XCTFail("the later adapter should have replaced the earlier one")
        }
    }

    func testLoadingWithThePlatformDefaultsRegistersEveryPortThatHasAnImplementation() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport, adapters: Adapters.platformDefault)
        var registered: Set<UInt32> = []
        for item in transport.sent {
            if case .registerPort(let id) = item {
                registered.insert(id)
            }
        }
        let expected: Set<UInt32> = [
            StandardPorts.Http.portId, StandardPorts.Kv.portId, StandardPorts.SecureStore.portId,
            StandardPorts.Fs.portId, StandardPorts.Clock.portId, StandardPorts.Rng.portId,
            StandardPorts.Log.portId, StandardPorts.Timer.portId, StandardPorts.Diagnostics.portId,
            StandardPorts.WebSocket.portId, StandardPorts.Sse.portId, StandardPorts.Db.portId,
        ]
        XCTAssertEqual(registered, expected, "Connectivity and Lifecycle have no methods the core calls")
        core.shutdown()
    }
}

// MARK: - Clock, Rng, Log

@MainActor
final class SystemAdapterTests: XCTestCase {
    func testClockReportsWallTimeAndAMonotonicCounter() throws {
        let core = try makeCore(FakeTransport())
        let adapter = ClockAdapter()
        XCTAssertEqual(adapter.portId, StandardPorts.Clock.portId)
        let impl = adapter.makePortImpl(core: core)
        let before = Int64(Date().timeIntervalSince1970 * 1000)
        let now = try Int64.undraDecoded(from: PortCaller.callSync(impl, StandardPorts.Clock.nowMs, []))
        XCTAssertGreaterThanOrEqual(now, before - 1)
        XCTAssertLessThan(now, before + 5000)
        let first = try UInt64.undraDecoded(from: PortCaller.callSync(impl, StandardPorts.Clock.monotonicNs, []))
        let second = try UInt64.undraDecoded(from: PortCaller.callSync(impl, StandardPorts.Clock.monotonicNs, []))
        XCTAssertGreaterThan(first, 0)
        XCTAssertLessThanOrEqual(first, second)
    }

    func testRngFillsTheRequestedLength() throws {
        let core = try makeCore(FakeTransport())
        let impl = RngAdapter().makePortImpl(core: core)
        func fill(_ count: UInt32) throws -> [UInt8] {
            let args = encodeArgs { (writer: inout UndraWriter) -> Void in
                writer.writeU32(count)
            }
            return try UndraBytes.undraDecoded(from: PortCaller.callSync(impl, StandardPorts.Rng.fill, args)).bytes
        }
        XCTAssertEqual(try fill(0), [])
        XCTAssertEqual(try fill(1).count, 1)
        let a = try fill(32)
        let b = try fill(32)
        XCTAssertEqual(a.count, 32)
        XCTAssertNotEqual(a, b, "two 256-bit draws must differ")
        XCTAssertNotEqual(a, [UInt8](repeating: 0, count: 32))
        XCTAssertEqual(try fill(100_000).count, 100_000)
    }

    func testRngRejectsAbsurdLengthsAndMalformedArguments() throws {
        let core = try makeCore(FakeTransport())
        let impl = RngAdapter().makePortImpl(core: core)
        let tooBig = encodeArgs { (writer: inout UndraWriter) -> Void in
            writer.writeU32(RngAdapter.maxFill + 1)
        }
        XCTAssertThrowsError(try PortCaller.callSync(impl, StandardPorts.Rng.fill, tooBig)) { error in
            XCTAssertTrue(error is PortAdapterError, "\(error)")
        }
        XCTAssertThrowsError(try PortCaller.callSync(impl, StandardPorts.Rng.fill, [1, 2])) { error in
            XCTAssertTrue(error is WireError, "\(error)")
        }
        XCTAssertThrowsError(try PortCaller.callSync(impl, StandardPorts.Rng.fill, [1, 0, 0, 0, 9])) { error in
            XCTAssertEqual(error as? WireError, WireError.trailingBytes(count: 1))
        }
    }

    func testLogAcceptsEveryLevelAndRejectsMalformedArguments() throws {
        let core = try makeCore(FakeTransport())
        let impl = LogAdapter().makePortImpl(core: core)
        for level in 0 ... 6 {
            let args = encodeArgs { (writer: inout UndraWriter) -> Void in
                writer.writeU8(UInt8(level))
                writer.writeString("undra::test")
                writer.writeString("hello \u{1F30A}")
            }
            XCTAssertEqual(try PortCaller.callSync(impl, StandardPorts.Log.log, args), [])
        }
        XCTAssertThrowsError(try PortCaller.callSync(impl, StandardPorts.Log.log, [2]))
    }

    func testLogLevelsMapOntoOSLogTypes() {
        XCTAssertEqual(UndraLog.osLogType(forUndraLevel: 0), .debug)
        XCTAssertEqual(UndraLog.osLogType(forUndraLevel: 2), .info)
        XCTAssertEqual(UndraLog.osLogType(forUndraLevel: 3), .default)
        XCTAssertEqual(UndraLog.osLogType(forUndraLevel: 4), .error)
        XCTAssertEqual(UndraLog.osLogType(forUndraLevel: 5), .fault)
        XCTAssertEqual(UndraLog.osLogType(forUndraLevel: 200), .fault)
    }
}

// MARK: - Timer

@MainActor
final class TimerAdapterTests: XCTestCase {
    private func setArgs(_ id: UInt32, _ delayMs: UInt64) -> [UInt8] {
        return encodeArgs { (writer: inout UndraWriter) -> Void in
            writer.writeU32(id)
            writer.writeU64(delayMs)
        }
    }

    func testTimersFireTheCoreInDueOrder() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let adapter = TimerAdapter()
        let impl = adapter.makePortImpl(core: core)
        adapter.attach(to: core)
        XCTAssertEqual(try PortCaller.callSync(impl, StandardPorts.Timer.set, setArgs(1, 150)), [])
        XCTAssertEqual(try PortCaller.callSync(impl, StandardPorts.Timer.set, setArgs(2, 20)), [])
        XCTAssertEqual(try PortCaller.callSync(impl, StandardPorts.Timer.set, setArgs(3, 80)), [])
        let fired = await waitUntil {
            return transport.sent.count == 3
        }
        XCTAssertTrue(fired)
        XCTAssertEqual(transport.sent, [.timerFired(2), .timerFired(3), .timerFired(1)])
    }

    func testAZeroDelayTimerStillFires() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let adapter = TimerAdapter()
        let impl = adapter.makePortImpl(core: core)
        adapter.attach(to: core)
        _ = try PortCaller.callSync(impl, StandardPorts.Timer.set, setArgs(9, 0))
        let fired = await waitUntil { transport.sent == [.timerFired(9)] }
        XCTAssertTrue(fired)
    }

    func testADetachedAdapterDropsPendingTimers() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let adapter = TimerAdapter()
        let impl = adapter.makePortImpl(core: core)
        adapter.attach(to: core)
        _ = try PortCaller.callSync(impl, StandardPorts.Timer.set, setArgs(5, 30))
        adapter.detach()
        try await Task.sleep(nanoseconds: 150_000_000)
        XCTAssertEqual(transport.sent, [])
    }

    func testAnAbsurdDelayIsCappedInsteadOfOverflowing() throws {
        let core = try makeCore(FakeTransport())
        let adapter = TimerAdapter()
        let impl = adapter.makePortImpl(core: core)
        XCTAssertEqual(try PortCaller.callSync(impl, StandardPorts.Timer.set, setArgs(1, UInt64.max)), [])
    }

    func testMalformedTimerArgumentsThrow() throws {
        let core = try makeCore(FakeTransport())
        let impl = TimerAdapter().makePortImpl(core: core)
        XCTAssertThrowsError(try PortCaller.callSync(impl, StandardPorts.Timer.set, [1, 0, 0, 0]))
    }
}

// MARK: - Kv and SecureStore

@MainActor
final class KeyValueAdapterTests: XCTestCase {
    private func makeDirectory() -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("undra-kv-tests-" + UUID().uuidString, isDirectory: true)
        addTeardownBlock {
            try? FileManager.default.removeItem(at: directory)
        }
        return directory
    }

    private func setArgs(_ key: String, _ value: [UInt8]) -> [UInt8] {
        return encodeArgs { (writer: inout UndraWriter) -> Void in
            writer.writeString(key)
            writer.writeBytes(value)
        }
    }

    private func get(_ impl: PortImpl?, _ key: String) async throws -> [UInt8]? {
        let reply = try await PortCaller.callAsync(impl, StandardPorts.Kv.get, keyArgs(key))
        return try Optional<UndraBytes>.undraDecoded(from: reply)?.bytes
    }

    private func list(_ impl: PortImpl?, _ prefix: String) async throws -> [String] {
        let reply = try await PortCaller.callAsync(impl, StandardPorts.Kv.list, keyArgs(prefix))
        return try [String].undraDecoded(from: reply)
    }

    func testKvStoresListsAndDeletesValues() async throws {
        let core = try makeCore(FakeTransport())
        let adapter = KvAdapter(directory: makeDirectory())
        XCTAssertEqual(adapter.portId, StandardPorts.Kv.portId)
        let impl = adapter.makePortImpl(core: core)

        let missing = try await get(impl, "a")
        XCTAssertNil(missing)
        let listing1 = try await list(impl, "")
        XCTAssertEqual(listing1, [])

        let setReply = try await PortCaller.callAsync(impl, StandardPorts.Kv.set, setArgs("a", [1, 2, 3]))
        XCTAssertEqual(setReply, [])
        let stored = try await get(impl, "a")
        XCTAssertEqual(stored, [1, 2, 3])
        _ = try await PortCaller.callAsync(impl, StandardPorts.Kv.set, setArgs("a", [9]))
        let overwritten = try await get(impl, "a")
        XCTAssertEqual(overwritten, [9])

        let unicodeKey = "b/caf\u{E9} \u{1F30A}"
        _ = try await PortCaller.callAsync(impl, StandardPorts.Kv.set, setArgs(unicodeKey, []))
        let empty = try await get(impl, unicodeKey)
        XCTAssertEqual(empty, [], "an empty value is not a missing value")
        let listing2 = try await list(impl, "")
        XCTAssertEqual(listing2, ["a", unicodeKey])
        let listing3 = try await list(impl, "b/")
        XCTAssertEqual(listing3, [unicodeKey])
        let listing4 = try await list(impl, "zzz")
        XCTAssertEqual(listing4, [])

        let deleteReply = try await PortCaller.callAsync(impl, StandardPorts.Kv.delete, keyArgs("a"))
        XCTAssertEqual(deleteReply, [])
        let deleted = try await get(impl, "a")
        XCTAssertNil(deleted)
        _ = try await PortCaller.callAsync(impl, StandardPorts.Kv.delete, keyArgs("a"))
        let listing5 = try await list(impl, "")
        XCTAssertEqual(listing5, [unicodeKey])
    }

    func testKvValuesSurviveANewAdapterOverTheSameDirectory() async throws {
        let core = try makeCore(FakeTransport())
        let directory = makeDirectory()
        let first = KvAdapter(directory: directory).makePortImpl(core: core)
        let big = [UInt8](repeating: 0x5A, count: 1_000_000)
        _ = try await PortCaller.callAsync(first, StandardPorts.Kv.set, setArgs("undra.query.queue", big))
        let second = KvAdapter(directory: directory).makePortImpl(core: core)
        let loaded = try await get(second, "undra.query.queue")
        XCTAssertEqual(loaded, big)
    }

    func testKvFileNamesAreStableAndKeysLiveInsideTheFiles() throws {
        XCTAssertEqual(FileKeyValueBackend.fileName(for: "a"), "af63dc4c8601ec8c-e40c292c")
        XCTAssertEqual(FileKeyValueBackend.fileName(for: ""), "cbf29ce484222325-811c9dc5")
        XCTAssertEqual(FileKeyValueBackend.fileName(for: "undra.query.queue"), "def1907793b60cec-f2daf06c")
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        try backend.set("a", [1])
        let file = directory.appendingPathComponent("af63dc4c8601ec8c-e40c292c")
        let bytes = [UInt8](try Data(contentsOf: file))
        XCTAssertEqual(bytes, [1, 0, 0, 0, 0x61, 1])
    }

    func testKvIgnoresFilesThatAreNotEntries() throws {
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        try backend.set("real", [1])
        try Data([1, 2]).write(to: directory.appendingPathComponent("junk-short"))
        try Data([0xFF, 0xFF, 0xFF, 0x7F, 1]).write(to: directory.appendingPathComponent("junk-length"))
        try Data([1, 0, 0, 0, 0xFF]).write(to: directory.appendingPathComponent("junk-utf8"))
        XCTAssertEqual(try backend.list(prefix: ""), ["real"])
        XCTAssertNil(try backend.get("never"))
    }

    /// The bytes of an entry file: `u32 key length, key, value`.
    private func entryBytes(_ key: String, _ value: [UInt8]) -> Data {
        var writer = UndraWriter(capacity: 4 + key.utf8.count + value.count)
        writer.writeString(key)
        writer.writeRaw(value)
        return Data(writer.finish())
    }

    func testKvTemporaryFileNamesAreDefinedOnceAndAreNotEntryNames() {
        let name = FileKeyValueBackend.fileName(for: "a")
        let temp = FileKeyValueBackend.temporaryName(for: name)
        XCTAssertTrue(temp.hasPrefix(name + "."), temp)
        XCTAssertTrue(temp.hasSuffix(".tmp"), temp)
        XCTAssertEqual(temp.count, name.count + 1 + 16 + 4, "<name>.<16 hex digits>.tmp, as the C++ store writes it")
        XCTAssertNotEqual(temp, FileKeyValueBackend.temporaryName(for: name), "each call is a fresh name")
        XCTAssertFalse(FileKeyValueBackend.isEntryName(temp))

        XCTAssertTrue(FileKeyValueBackend.isEntryName(name))
        XCTAssertTrue(FileKeyValueBackend.isEntryName("cbf29ce484222325-811c9dc5"))
        for notAnEntry in [
            "", ".", "..", ".DS_Store", "junk-short", name + ".tmp", "." + name, name + "0",
            "AF63DC4C8601EC8C-E40C292C", "af63dc4c8601ec8c_e40c292c", "af63dc4c8601ec8-ce40c292c",
            "af63dc4c8601ec8c-e40c292g", ".dat.nosync1234.AbCdEf",
        ] {
            XCTAssertFalse(FileKeyValueBackend.isEntryName(notAnEntry), notAnEntry)
        }
    }

    func testKvSetLeavesOnlyTheSealedEntryWithOwnerOnlyPermissions() throws {
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        try backend.set("a", [1])
        try backend.set("a", [2, 3])
        try backend.set("b", [])
        XCTAssertEqual(
            try FileManager.default.contentsOfDirectory(atPath: directory.path).sorted(),
            [FileKeyValueBackend.fileName(for: "a"), FileKeyValueBackend.fileName(for: "b")].sorted(),
            "no temporary file is left behind"
        )
        var info = stat()
        XCTAssertEqual(stat(directory.appendingPathComponent(FileKeyValueBackend.fileName(for: "a")).path, &info), 0)
        XCTAssertEqual(info.st_mode & 0o777, 0o600)
        XCTAssertEqual(try backend.get("a"), [2, 3])
        XCTAssertEqual(try backend.get("b"), [])
    }

    func testKvListSkipsTheTemporaryFilesOfAKilledWriter() throws {
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        try backend.set("a", [1])
        let name = FileKeyValueBackend.fileName(for: "a")
        // A complete copy of the entry (killed between the write and the rename), the same cut
        // short, an older Foundation-style temporary, and a dot file.
        let whole = entryBytes("a", [1])
        try whole.write(to: directory.appendingPathComponent(name + ".0123456789abcdef.tmp"))
        try whole.prefix(3).write(to: directory.appendingPathComponent(name + ".fedcba9876543210.tmp"))
        try entryBytes("ghost", [9]).write(to: directory.appendingPathComponent(FileKeyValueBackend.fileName(for: "ghost") + ".00000000000000aa.tmp"))
        try whole.write(to: directory.appendingPathComponent(".dat.nosync0001.AbCdEf"))
        try whole.write(to: directory.appendingPathComponent(".hidden"))
        XCTAssertEqual(try backend.list(prefix: ""), ["a"], "once, and no key that only a temporary file holds")
        XCTAssertEqual(try backend.list(prefix: "g"), [])
        XCTAssertNil(try backend.get("ghost"))

        try backend.delete("a")
        XCTAssertEqual(try backend.list(prefix: ""), [], "a leftover does not bring a deleted key back")
        XCTAssertNil(try backend.get("a"))
    }

    func testKvListSkipsAnEntryWhoseNameIsNotItsKeysName() throws {
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        try backend.set("real", [1])
        // A copy of an entry under another entry-shaped name is not a sealed entry of its key.
        try entryBytes("copy", [2]).write(to: directory.appendingPathComponent("0000000000000000-00000000"))
        XCTAssertEqual(try backend.list(prefix: ""), ["real"])
        XCTAssertNil(try backend.get("copy"))
    }

    func testKvListSkipsAnEntryNamedDirectory() throws {
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        try backend.set("real", [1])
        try FileManager.default.createDirectory(
            at: directory.appendingPathComponent("0000000000000000-00000000"),
            withIntermediateDirectories: false
        )
        XCTAssertEqual(try backend.list(prefix: ""), ["real"])
    }

    /// A damaged header is `.corrupt` (ADR-049), not "no value": entries are sealed, so it is damage,
    /// and "no value" would let the query client take a stored queue for an empty one. The key is
    /// not listed, `delete` removes the file and `set` replaces it.
    func testKvGetOfADamagedEntryIsCorruptAndTheKeyCanBeWrittenAgain() throws {
        let directory = makeDirectory()
        let backend = FileKeyValueBackend(directory: directory)
        let file = directory.appendingPathComponent(FileKeyValueBackend.fileName(for: "k"))
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let damaged: [(String, Data)] = [
            ("empty", Data()),
            ("two bytes", Data([1, 0])),
            ("header only", Data([1, 0, 0, 0])),
            ("key cut short", entryBytes("kkkkkkkk", [1, 2, 3]).prefix(7)),
            ("absurd key length", Data([0xFF, 0xFF, 0xFF, 0x7F, 0x6B, 1])),
            ("key not UTF-8", Data([1, 0, 0, 0, 0xFF, 7])),
        ]
        for (label, bytes) in damaged {
            try bytes.write(to: file)
            XCTAssertThrowsError(try backend.get("k"), label) { error in
                guard case .corrupt(let reason)? = error as? StorageError else {
                    return XCTFail("\(label): expected Corrupt, got \(error)")
                }
                XCTAssertTrue(reason.contains("\"k\""), reason)
            }
            XCTAssertEqual(try backend.list(prefix: ""), [], label)
            try backend.delete("k") // removes the damaged file, never throws
            XCTAssertNil(try backend.get("k"), "\(label): deleted")
        }
        try Data([1, 0]).write(to: file)
        try backend.set("k", [5])
        XCTAssertEqual(try backend.get("k"), [5], "a write replaces the damaged file")
        XCTAssertEqual(try backend.list(prefix: ""), ["k"])
    }

    func testKvTemporaryAndDamagedFilesAreInvisibleThroughThePort() async throws {
        let core = try makeCore(FakeTransport())
        let directory = makeDirectory()
        let impl = KvAdapter(directory: directory).makePortImpl(core: core)
        _ = try await PortCaller.callAsync(impl, StandardPorts.Kv.set, setArgs("a", [1]))
        let name = FileKeyValueBackend.fileName(for: "a")
        try entryBytes("a", [1]).write(to: directory.appendingPathComponent(name + ".0123456789abcdef.tmp"))
        try Data([0xFF]).write(to: directory.appendingPathComponent(FileKeyValueBackend.fileName(for: "b")))
        let keys = try await list(impl, "")
        XCTAssertEqual(keys, ["a"])
        // The damaged entry of "b" is not listed; reading it answers Corrupt (port status 1, ADR-049).
        do {
            _ = try await get(impl, "b")
            XCTFail("the damaged entry of b must answer Corrupt")
        } catch let error as UndraPortError {
            guard case .corrupt = try StorageError.undraDecoded(from: error.body) else {
                return XCTFail("expected Corrupt")
            }
        }
        let a = try await get(impl, "a")
        XCTAssertEqual(a, [1])
    }

    func testKvRejectsMalformedArguments() async throws {
        let core = try makeCore(FakeTransport())
        let impl = KvAdapter(directory: makeDirectory()).makePortImpl(core: core)
        do {
            _ = try await PortCaller.callAsync(impl, StandardPorts.Kv.get, [1])
            XCTFail("a truncated key must not be accepted")
        } catch {
            XCTAssertTrue(error is WireError, "\(error)")
        }
    }

    func testSecureStoreUsesItsOwnMethodIdsOverAnyBackend() async throws {
        let core = try makeCore(FakeTransport())
        let adapter = SecureStoreAdapter(backend: MemoryBackend())
        XCTAssertEqual(adapter.portId, StandardPorts.SecureStore.portId)
        let impl = adapter.makePortImpl(core: core)
        _ = try await PortCaller.callAsync(impl, StandardPorts.SecureStore.set, setArgs("token", [7, 7]))
        let reply = try await PortCaller.callAsync(impl, StandardPorts.SecureStore.get, keyArgs("token"))
        XCTAssertEqual(try Optional<UndraBytes>.undraDecoded(from: reply)?.bytes, [7, 7])
        let names = try await PortCaller.callAsync(impl, StandardPorts.SecureStore.list, keyArgs("to"))
        XCTAssertEqual(try [String].undraDecoded(from: names), ["token"])
        _ = try await PortCaller.callAsync(impl, StandardPorts.SecureStore.delete, keyArgs("token"))
        let after = try await PortCaller.callAsync(impl, StandardPorts.SecureStore.get, keyArgs("token"))
        XCTAssertEqual(after, [0])
        // Kv's ids are not served by the SecureStore table.
        do {
            _ = try await PortCaller.callAsync(impl, StandardPorts.Kv.get, keyArgs("token"))
            XCTFail("the Kv method id must not be served by SecureStore")
        } catch {
            XCTAssertTrue(error is TestFailure)
        }
    }

    func testABackendFailureSurfacesAsATypedStorageError() async throws {
        let core = try makeCore(FakeTransport())
        let impl = SecureStoreAdapter(backend: BrokenBackend()).makePortImpl(core: core)
        do {
            _ = try await PortCaller.callAsync(impl, StandardPorts.SecureStore.get, keyArgs("x"))
            XCTFail("a broken backend must throw")
        } catch let error as UndraPortError {
            XCTAssertEqual(try StorageError.undraDecoded(from: error.body), .io("broken"))
        } catch {
            XCTFail("expected an UndraPortError carrying a StorageError, got \(error)")
        }
        // Through the runtime the same failure answers with port status 1 and the encoded error
        // (ADR-049), not "unavailable".
        let transport = FakeTransport()
        let running = try makeCore(transport)
        running.registerPort(StandardPorts.SecureStore.portId, impl!)
        let outcome = transport.callPort(
            portId: StandardPorts.SecureStore.portId,
            methodId: StandardPorts.SecureStore.get,
            portCallId: 3,
            args: keyArgs("x")
        )
        XCTAssertEqual(outcome, .async)
        let answered = await waitUntil { transport.portReplies.count == 1 }
        XCTAssertTrue(answered)
        XCTAssertEqual(
            transport.portReplies[0],
            Wire.PortReply(portCallId: 3, status: .error, body: ArraySlice(StorageError.io("broken").undraEncoded()))
        )
    }
}

// MARK: - Fs

@MainActor
final class FsAdapterTests: XCTestCase {
    private func makeRoot() -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("undra-fs-tests-" + UUID().uuidString, isDirectory: true)
        addTeardownBlock {
            try? FileManager.default.removeItem(at: root)
        }
        return root
    }

    private func writeArgs(_ path: String, _ data: [UInt8]) -> [UInt8] {
        return encodeArgs { (writer: inout UndraWriter) -> Void in
            writer.writeString(path)
            writer.writeBytes(data)
        }
    }

    /// The `FsError` a failed call carries.
    private func fsError(_ body: () async throws -> [UInt8], file: StaticString = #filePath, line: UInt = #line) async -> FsError? {
        do {
            _ = try await body()
            XCTFail("expected an Fs error", file: file, line: line)
            return nil
        } catch let error as UndraPortError {
            return try? FsError.undraDecoded(from: error.body)
        } catch {
            XCTFail("expected an UndraPortError, got \(error)", file: file, line: line)
            return nil
        }
    }

    func testWriteReadListAndDelete() async throws {
        let core = try makeCore(FakeTransport())
        let adapter = FsAdapter(root: makeRoot())
        XCTAssertEqual(adapter.portId, StandardPorts.Fs.portId)
        let impl = adapter.makePortImpl(core: core)

        let written = try await PortCaller.callAsync(impl, StandardPorts.Fs.write, writeArgs("notes/2026/a.txt", [1, 2, 3]))
        XCTAssertEqual(written, [], "write creates the missing directories")
        _ = try await PortCaller.callAsync(impl, StandardPorts.Fs.write, writeArgs("notes/b.txt", []))
        _ = try await PortCaller.callAsync(impl, StandardPorts.Fs.write, writeArgs("top", [9]))

        let read = try await PortCaller.callAsync(impl, StandardPorts.Fs.read, keyArgs("notes/2026/a.txt"))
        XCTAssertEqual(try UndraBytes.undraDecoded(from: read).bytes, [1, 2, 3])
        let empty = try await PortCaller.callAsync(impl, StandardPorts.Fs.read, keyArgs("notes/b.txt"))
        XCTAssertEqual(try UndraBytes.undraDecoded(from: empty).bytes, [])

        let root = try await PortCaller.callAsync(impl, StandardPorts.Fs.list, keyArgs(""))
        XCTAssertEqual(try [String].undraDecoded(from: root), ["notes", "top"])
        let notes = try await PortCaller.callAsync(impl, StandardPorts.Fs.list, keyArgs("/notes/"))
        XCTAssertEqual(try [String].undraDecoded(from: notes), ["2026", "b.txt"])

        _ = try await PortCaller.callAsync(impl, StandardPorts.Fs.delete, keyArgs("notes/b.txt"))
        let after = try await PortCaller.callAsync(impl, StandardPorts.Fs.list, keyArgs("notes"))
        XCTAssertEqual(try [String].undraDecoded(from: after), ["2026"])
        _ = try await PortCaller.callAsync(impl, StandardPorts.Fs.delete, keyArgs("notes"))
        let gone = try await PortCaller.callAsync(impl, StandardPorts.Fs.list, keyArgs(""))
        XCTAssertEqual(try [String].undraDecoded(from: gone), ["top"], "delete removes a directory tree")
    }

    func testMissingPathsAreNotFound() async throws {
        let core = try makeCore(FakeTransport())
        let impl = FsAdapter(root: makeRoot()).makePortImpl(core: core)
        for method in [StandardPorts.Fs.read, StandardPorts.Fs.delete, StandardPorts.Fs.list] {
            let error = await fsError {
                return try await PortCaller.callAsync(impl, method, keyArgs("nope/missing"))
            }
            XCTAssertEqual(error, .notFound)
        }
    }

    func testPathsCannotEscapeTheRoot() async throws {
        let core = try makeCore(FakeTransport())
        let root = makeRoot()
        let impl = FsAdapter(root: root).makePortImpl(core: core)
        for path in ["../x", "a/../../x", "..", "a/.."] {
            for method in [StandardPorts.Fs.read, StandardPorts.Fs.delete, StandardPorts.Fs.list] {
                let error = await fsError {
                    return try await PortCaller.callAsync(impl, method, keyArgs(path))
                }
                XCTAssertEqual(error, .denied, "\(path)")
            }
            let error = await fsError {
                return try await PortCaller.callAsync(impl, StandardPorts.Fs.write, writeArgs(path, [1]))
            }
            XCTAssertEqual(error, .denied, "\(path)")
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.deletingLastPathComponent().appendingPathComponent("x").path))
    }

    func testWrongKindsOfPathAreIoErrors() async throws {
        let core = try makeCore(FakeTransport())
        let impl = FsAdapter(root: makeRoot()).makePortImpl(core: core)
        _ = try await PortCaller.callAsync(impl, StandardPorts.Fs.write, writeArgs("dir/file", [1]))
        let readDirectory = await fsError {
            return try await PortCaller.callAsync(impl, StandardPorts.Fs.read, keyArgs("dir"))
        }
        guard case .io? = readDirectory else {
            return XCTFail("reading a directory should be an Io error, got \(String(describing: readDirectory))")
        }
        let listFile = await fsError {
            return try await PortCaller.callAsync(impl, StandardPorts.Fs.list, keyArgs("dir/file"))
        }
        guard case .io? = listFile else {
            return XCTFail("listing a file should be an Io error, got \(String(describing: listFile))")
        }
        let writeRoot = await fsError {
            return try await PortCaller.callAsync(impl, StandardPorts.Fs.write, writeArgs("", [1]))
        }
        guard case .io? = writeRoot else {
            return XCTFail("writing the root should be an Io error, got \(String(describing: writeRoot))")
        }
        let deleteRoot = await fsError {
            return try await PortCaller.callAsync(impl, StandardPorts.Fs.delete, keyArgs("."))
        }
        XCTAssertEqual(deleteRoot, .denied)
    }

    func testErrnoMapsToFsErrors() {
        XCTAssertEqual(FsAdapter.fsError(errno: ENOENT), .notFound)
        XCTAssertEqual(FsAdapter.fsError(errno: ENOTDIR), .notFound)
        XCTAssertEqual(FsAdapter.fsError(errno: EACCES), .denied)
        XCTAssertEqual(FsAdapter.fsError(errno: EPERM), .denied)
        XCTAssertEqual(FsAdapter.fsError(errno: ELOOP), .denied)
        // A full disk or quota is `Full` (ADR-049).
        XCTAssertEqual(FsAdapter.fsError(errno: ENOSPC), .full)
        XCTAssertEqual(FsAdapter.fsError(errno: EDQUOT), .full)
        XCTAssertEqual(FsAdapter.fsError(errno: EIO), .io(String(cString: strerror(EIO))))
        XCTAssertEqual(FsAdapter.map(PosixError(code: ENOSPC)), .full)
        XCTAssertEqual(FsAdapter.map(PosixError(code: ENOENT)), .notFound)
    }

    func testFoundationErrorsMapToFsErrors() {
        XCTAssertEqual(FsAdapter.map(CocoaError(.fileNoSuchFile)), .notFound)
        XCTAssertEqual(FsAdapter.map(CocoaError(.fileReadNoSuchFile)), .notFound)
        XCTAssertEqual(FsAdapter.map(CocoaError(.fileReadNoPermission)), .denied)
        XCTAssertEqual(FsAdapter.map(CocoaError(.fileWriteNoPermission)), .denied)
        // Out of space is `Full` (ADR-049), however Foundation spells it.
        XCTAssertEqual(FsAdapter.map(CocoaError(.fileWriteOutOfSpace)), .full)
        XCTAssertEqual(FsAdapter.map(POSIXError(.ENOSPC)), .full)
        XCTAssertEqual(FsAdapter.map(POSIXError(.EDQUOT)), .full)
        XCTAssertEqual(
            FsAdapter.map(NSError(domain: NSCocoaErrorDomain, code: NSFileWriteUnknownError, userInfo: [NSUnderlyingErrorKey: POSIXError(.ENOSPC)])),
            .full
        )
        guard case .io? = Optional(FsAdapter.map(CocoaError(.fileWriteUnknown))) else {
            return XCTFail("other Cocoa errors are Io errors")
        }
        guard case .io? = Optional(FsAdapter.map(TestFailure(description: "x"))) else {
            return XCTFail("other errors are Io errors")
        }
    }
}

// MARK: - Http

/// Answers every request from a closure, so `HttpAdapter` can be tested without a network.
final class StubURLProtocol: URLProtocol {
    enum Behavior: Sendable {
        case respond(@Sendable (URLRequest) throws -> (HTTPURLResponse, Data))
        case hang
    }

    static let behavior = Guarded<Behavior>(.hang)
    static let lastRequest = Guarded<URLRequest?>(nil)
    static let lastBody = Guarded<[UInt8]?>(nil)

    override class func canInit(with request: URLRequest) -> Bool {
        return true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest {
        return request
    }

    override func startLoading() {
        let current = StubURLProtocol.behavior.withLock { (value: inout Behavior) -> Behavior in
            return value
        }
        StubURLProtocol.lastRequest.withLock { (value: inout URLRequest?) -> Void in
            value = request
        }
        StubURLProtocol.lastBody.withLock { (value: inout [UInt8]?) -> Void in
            value = StubURLProtocol.readBody(of: request)
        }
        switch current {
        case .hang:
            return
        case .respond(let handler):
            do {
                let (response, data) = try handler(request)
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: data)
                client?.urlProtocolDidFinishLoading(self)
            } catch {
                client?.urlProtocol(self, didFailWithError: error)
            }
        }
    }

    override func stopLoading() {}

    static func readBody(of request: URLRequest) -> [UInt8]? {
        if let body = request.httpBody {
            return [UInt8](body)
        }
        guard let stream = request.httpBodyStream else {
            return nil
        }
        stream.open()
        defer {
            stream.close()
        }
        var result: [UInt8] = []
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            let count = stream.read(&buffer, maxLength: buffer.count)
            if count <= 0 {
                break
            }
            result.append(contentsOf: buffer[0 ..< count])
        }
        return result
    }

    static func makeSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        return URLSession(configuration: configuration)
    }
}

@MainActor
final class HttpAdapterTests: XCTestCase {
    private func requestArgs(_ request: HttpRequest) -> [UInt8] {
        return request.undraEncoded()
    }

    private func respond(_ handler: @escaping @Sendable (URLRequest) throws -> (HTTPURLResponse, Data)) {
        StubURLProtocol.behavior.withLock { (value: inout StubURLProtocol.Behavior) -> Void in
            value = .respond(handler)
        }
    }

    private func perform(_ request: HttpRequest) async throws -> HttpResponse {
        let adapter = HttpAdapter(session: StubURLProtocol.makeSession())
        let core = try makeCore(FakeTransport())
        let impl = adapter.makePortImpl(core: core)
        let reply = try await PortCaller.callAsync(impl, StandardPorts.Http.request, requestArgs(request))
        return try HttpResponse.undraDecoded(from: reply)
    }

    private func httpError(_ request: HttpRequest, file: StaticString = #filePath, line: UInt = #line) async -> HttpError? {
        do {
            _ = try await perform(request)
            XCTFail("expected an Http error", file: file, line: line)
            return nil
        } catch let error as UndraPortError {
            return try? HttpError.undraDecoded(from: error.body)
        } catch {
            XCTFail("expected an UndraPortError, got \(error)", file: file, line: line)
            return nil
        }
    }

    func testAPostWithHeadersBodyAndTimeoutReachesTheSessionAndTheResponseComesBack() async throws {
        respond { request in
            let url = try XCTUnwrap(request.url)
            let response = try XCTUnwrap(HTTPURLResponse(
                url: url,
                statusCode: 201,
                httpVersion: "HTTP/1.1",
                headerFields: ["B-Header": "2", "A-Header": "1"]
            ))
            return (response, Data([1, 2, 3]))
        }
        let response = try await perform(HttpRequest(
            method: .post,
            url: "https://example.com/todos?x=1",
            headers: [Header(name: "X-Test", value: "v")],
            body: [9, 8, 7],
            timeoutMs: 2500
        ))
        XCTAssertEqual(response.status, 201)
        XCTAssertEqual(response.body, [1, 2, 3])
        XCTAssertEqual(response.headers, [Header(name: "A-Header", value: "1"), Header(name: "B-Header", value: "2")])

        let seen = StubURLProtocol.lastRequest.withLock { (value: inout URLRequest?) -> URLRequest? in
            return value
        }
        let request = try XCTUnwrap(seen)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.absoluteString, "https://example.com/todos?x=1")
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Test"), "v")
        XCTAssertEqual(request.timeoutInterval, 2.5, accuracy: 0.0001)
        let body = StubURLProtocol.lastBody.withLock { (value: inout [UInt8]?) -> [UInt8]? in
            return value
        }
        XCTAssertEqual(body, [9, 8, 7])
    }

    func testEveryMethodIsSpelledCorrectlyOnTheRequestLine() async throws {
        respond { request in
            let url = try XCTUnwrap(request.url)
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: 204, httpVersion: nil, headerFields: nil))
            return (response, Data())
        }
        for method in HttpMethod.allCases {
            _ = try await perform(HttpRequest(method: method, url: "https://example.com/", headers: [], body: nil, timeoutMs: nil))
            let seen = StubURLProtocol.lastRequest.withLock { (value: inout URLRequest?) -> URLRequest? in
                return value
            }
            XCTAssertEqual(seen?.httpMethod, method.name)
        }
    }

    func testAnErrorStatusIsAnOrdinaryResponse() async throws {
        respond { request in
            let url = try XCTUnwrap(request.url)
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: 404, httpVersion: nil, headerFields: nil))
            return (response, Data("nope".utf8))
        }
        let response = try await perform(HttpRequest(method: .get, url: "https://example.com/x", headers: [], body: nil, timeoutMs: nil))
        XCTAssertEqual(response.status, 404)
        XCTAssertEqual(response.body, Array("nope".utf8))
        XCTAssertEqual(response.headers, [])
    }

    func testUnusableURLsAreInvalidUrlErrors() async throws {
        for url in ["", "not a url", "ftp://example.com/x", "https://", "//example.com"] {
            let error = await httpError(HttpRequest(method: .get, url: url, headers: [], body: nil, timeoutMs: nil))
            XCTAssertEqual(error, .invalidUrl(url), url)
        }
    }

    func testURLErrorsMapToHttpErrors() async throws {
        let cases: [(URLError.Code, HttpError?)] = [
            (.timedOut, .timeout),
            (.cancelled, .cancelled),
            (.badURL, .invalidUrl("https://example.com/")),
            (.unsupportedURL, .invalidUrl("https://example.com/")),
            (.notConnectedToInternet, nil),
        ]
        for (code, expected) in cases {
            respond { _ in
                throw URLError(code)
            }
            let error = await httpError(HttpRequest(method: .get, url: "https://example.com/", headers: [], body: nil, timeoutMs: nil))
            if let expected = expected {
                XCTAssertEqual(error, expected, "\(code)")
            } else {
                guard case .network(let text)? = error else {
                    return XCTFail("\(code) should map to a Network error, got \(String(describing: error))")
                }
                XCTAssertFalse(text.isEmpty)
            }
        }
    }

    func testCancellingTheTaskAnswersCancelled() async throws {
        StubURLProtocol.behavior.withLock { (value: inout StubURLProtocol.Behavior) -> Void in
            value = .hang
        }
        let request = HttpRequest(method: .get, url: "https://example.com/slow", headers: [], body: nil, timeoutMs: nil)
        let task = Task { () -> HttpResponse in
            return try await self.perform(request)
        }
        let started = await waitUntil {
            return StubURLProtocol.lastRequest.withLock { (value: inout URLRequest?) -> Bool in
                return value?.url?.path == "/slow"
            }
        }
        XCTAssertTrue(started)
        task.cancel()
        let result = await task.result
        switch result {
        case .success:
            XCTFail("a cancelled request must not succeed")
        case .failure(let error):
            guard let portError = error as? UndraPortError else {
                return XCTFail("expected an UndraPortError, got \(error)")
            }
            XCTAssertEqual(try? HttpError.undraDecoded(from: portError.body), .cancelled)
        }
    }

    func testARequestWithAZeroTimeoutIsNotAnUnlimitedOne() async throws {
        respond { request in
            let url = try XCTUnwrap(request.url)
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil))
            return (response, Data())
        }
        _ = try await perform(HttpRequest(method: .get, url: "https://example.com/", headers: [], body: nil, timeoutMs: 0))
        let seen = StubURLProtocol.lastRequest.withLock { (value: inout URLRequest?) -> URLRequest? in
            return value
        }
        XCTAssertGreaterThan(try XCTUnwrap(seen).timeoutInterval, 0)
        XCTAssertLessThan(try XCTUnwrap(seen).timeoutInterval, 1)
    }
}

// MARK: - Connectivity and lifecycle

@MainActor
final class EventAdapterTests: XCTestCase {
    func testClassificationPrefersTheMostSpecificInterface() {
        func classify(
            _ status: NWPath.Status,
            wifi: Bool = false,
            cellular: Bool = false,
            wired: Bool = false
        ) -> (online: Bool, kind: NetKind) {
            return ConnectivityAdapter.classify(
                status: status,
                usesWifi: wifi,
                usesCellular: cellular,
                usesWired: wired
            )
        }
        XCTAssertEqual(classify(.unsatisfied, wifi: true).online, false)
        XCTAssertEqual(classify(.unsatisfied, wifi: true).kind, .disconnected)
        XCTAssertEqual(classify(.requiresConnection).kind, .disconnected)
        XCTAssertEqual(classify(.satisfied, wifi: true, cellular: true).kind, .wifi)
        XCTAssertEqual(classify(.satisfied, cellular: true, wired: true).kind, .cellular)
        XCTAssertEqual(classify(.satisfied, wired: true).kind, .wired)
        XCTAssertEqual(classify(.satisfied).kind, .unknown)
        XCTAssertEqual(classify(.satisfied).online, true)
    }

    func testTheConnectivityMonitorReportsTheCurrentPathToTheCore() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let adapter = ConnectivityAdapter()
        XCTAssertNil(adapter.makePortImpl(core: core))
        adapter.attach(to: core)
        let arrived = await waitUntil {
            return transport.sent.contains { (item: FakeTransport.Sent) -> Bool in
                if case .event(let port, let method, let payload) = item {
                    return port == StandardPorts.Connectivity.portId
                        && method == StandardPorts.Connectivity.changed
                        && payload.count == 3
                }
                return false
            }
        }
        adapter.detach()
        try XCTSkipUnless(arrived, "NWPathMonitor delivered no path in this environment")
    }

    func testAttachingTwiceReplacesTheMonitor() throws {
        let core = try makeCore(FakeTransport())
        let adapter = ConnectivityAdapter()
        adapter.attach(to: core)
        adapter.attach(to: core)
        adapter.detach()
        adapter.detach()
    }

    func testLifecycleEventsReachTheCore() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let lifecycle = UndraLifecycle(core: core)
        lifecycle.changed(.active)
        lifecycle.changed(.inactive)
        lifecycle.changed(.background)
        let port = StandardPorts.Lifecycle.portId
        let method = StandardPorts.Lifecycle.changed
        XCTAssertEqual(transport.sent, [
            .event(port, method, [0, 0]),
            .event(port, method, [1, 0]),
            .event(port, method, [2, 0]),
        ])
    }
}
