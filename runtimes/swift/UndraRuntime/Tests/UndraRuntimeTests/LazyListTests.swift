import Foundation
import XCTest
@testable import UndraRuntime

/// `UndraLazyList` (ADR-043 decision 3): the cache, the requests, the version rules and the window, over a fake core that serves
/// pages. Rows are `Int32`s whose value is their index until a test changes them; pages are 50 rows.
@available(iOS 17, macOS 14, *)
@MainActor
final class LazyListTests: XCTestCase {
    // MARK: Count and rows

    func testAListIsEmptyUntilItsSignalHasAValue() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let list = UndraLazyList<Int32>(core: core)
        XCTAssertEqual(list.count, 0)
        XCTAssertEqual(list.startIndex, 0)
        XCTAssertEqual(list.endIndex, 0)
        XCTAssertTrue(list.isEmpty)
        XCTAssertNil(list[0])
        XCTAssertEqual(list.pageSize, 50)
        XCTAssertEqual(list.maxCachedPages, 24)
        XCTAssertEqual(transport.calls.count, 0)
    }

    func testTheSignalsValueSetsTheLengthAndAsksForNothing() throws {
        let rig = try LazyRig(rows: 120)
        XCTAssertEqual(rig.list.count, 120)
        XCTAssertEqual(rig.list.endIndex, 120)
        XCTAssertEqual(rig.turns.scheduled, 0)
        XCTAssertEqual(rig.server.offsets, [])
    }

    func testReadingARowRequestsItsPageAndOnePageOfPrefetchOnEachSideOnce() throws {
        let rig = try LazyRig(rows: 500)
        XCTAssertNil(rig.list[260])
        XCTAssertEqual(rig.server.offsets, [], "nothing is sent from inside a read")
        XCTAssertEqual(rig.turns.pending.count, 1)
        rig.turns.run()
        XCTAssertEqual(rig.server.pages(), [4, 5, 6])
        XCTAssertEqual(rig.server.limits, [50, 50, 50])
        XCTAssertEqual(rig.list[260], 260)
        XCTAssertEqual(rig.list[205], 205, "the page before was prefetched")
        XCTAssertEqual(rig.list[349], 349, "the page after was prefetched")
        XCTAssertNil(rig.list[199], "two pages away was not")
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [4, 5, 6, 2, 3, 7], "reading the prefetched pages asked for their neighbours, each page once")
    }

    func testEveryPageCallIsCountedInTheStats() async throws {
        let rig = try LazyRig(rows: 500)
        XCTAssertEqual(rig.core.stats().hostPageCalls, 0)
        rig.read(260)
        XCTAssertEqual(rig.core.stats().hostPageCalls, 3, "three pages, one call each")
        XCTAssertEqual(rig.core.stats().hostPageCalls, rig.server.offsets.count)
        _ = rig.list[1]
        XCTAssertEqual(rig.core.stats().hostPageCalls, 3, "a read only queues")
        // The asynchronous path counts too.
        let remote = try LazyRig(rows: 100, direct: false)
        _ = remote.list[0]
        remote.turns.run()
        let asked = await waitUntil { remote.core.stats().hostPageCalls == 2 }
        XCTAssertTrue(asked)
    }

    func testFirstAndLastPagesPrefetchOnlyTheirOneNeighbour() throws {
        let rig = try LazyRig(rows: 120)
        rig.read(0)
        XCTAssertEqual(rig.server.pages(), [0, 1])
        rig.server.forgetCalls()
        rig.read(119)
        XCTAssertEqual(rig.server.pages(), [2], "page 1 was cached, page 2 is the last")
        XCTAssertEqual(rig.list[119], 119)
        XCTAssertEqual(rig.list[100], 100, "the last page holds 20 rows")
    }

    func testAnIndexOutsideTheListReturnsNilAndRequestsNothing() throws {
        let rig = try LazyRig(rows: 120)
        XCTAssertNil(rig.list[-1])
        XCTAssertNil(rig.list[120])
        XCTAssertNil(rig.list[Int.max])
        XCTAssertNil(rig.list[Int.min])
        XCTAssertEqual(rig.turns.scheduled, 0)
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.offsets, [])
        XCTAssertEqual(rig.transport.calls.count, 0)
    }

    func testRowsOfAnEmptyListAreNil() throws {
        let rig = try LazyRig(rows: 0)
        XCTAssertEqual(rig.list.count, 0)
        XCTAssertNil(rig.list[0])
        rig.prefetchAndRun(0 ..< 10)
        XCTAssertEqual(rig.server.offsets, [])
    }

    func testTheRequestsOfOneTurnAreOneBatchInPageOrderWithoutDuplicates() throws {
        let rig = try LazyRig(rows: 500)
        _ = [rig.list[120], rig.list[0], rig.list[49], rig.list[50], rig.list[99], rig.list[121], rig.list[10]]
        XCTAssertEqual(rig.turns.scheduled, 1, "one flush however many reads")
        rig.turns.run()
        XCTAssertEqual(rig.server.offsets, [0, 50, 100, 150], "each page once, ascending")
        XCTAssertEqual(rig.server.pages(), [0, 1, 2, 3])
        XCTAssertEqual(rig.turns.scheduled, 1)
    }

    func testRowsAreTheCoreSRowsAcrossPageBoundaries() throws {
        let rig = try LazyRig(rows: 130)
        rig.read(0, 49, 50, 99, 100, 129)
        XCTAssertEqual(rig.read(0, 49, 50, 99, 100, 129), [0, 49, 50, 99, 100, 129])
        XCTAssertEqual((0 ..< 130).map { rig.list[$0] }, (0 ..< 130).map { Optional(Int32($0)) })
    }

    func testACachedPageIsNotRequestedAgain() throws {
        let rig = try LazyRig(rows: 500)
        rig.read(60)
        let sent = rig.server.offsets
        for _ in 0 ..< 5 {
            XCTAssertEqual(rig.list[60], 60)
            XCTAssertEqual(rig.list[75], 75)
        }
        XCTAssertEqual(rig.turns.pending.count, 0)
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.offsets, sent)
    }

    // MARK: Prefetch

    func testPrefetchRequestsThePagesOfARange() throws {
        let rig = try LazyRig(rows: 1000)
        rig.list.prefetch(120 ..< 260)
        XCTAssertEqual(rig.turns.pending.count, 1)
        rig.turns.run()
        XCTAssertEqual(rig.server.pages(), [2, 3, 4, 5], "no neighbours: only the pages of the range")
        XCTAssertEqual(rig.list[130], 130)
    }

    func testPrefetchOutsideTheListIsClampedOrIgnored() throws {
        let rig = try LazyRig(rows: 120)
        rig.prefetchAndRun(-50 ..< 10)
        XCTAssertEqual(rig.server.pages(), [0])
        rig.server.forgetCalls()
        rig.prefetchAndRun(100 ..< 100_000)
        XCTAssertEqual(rig.server.pages(), [2])
        rig.server.forgetCalls()
        rig.prefetchAndRun(500 ..< 600)
        rig.prefetchAndRun(5 ..< 5)
        rig.prefetchAndRun(Int.min ..< Int.min + 3)
        XCTAssertEqual(rig.server.pages(), [])
    }

    func testPrefetchOfAHugeRangeAsksForNoMoreThanTheCacheHolds() throws {
        let rig = try LazyRig(rows: 5000, maxCachedPages: 6)
        rig.prefetchAndRun(0 ..< Int.max)
        XCTAssertEqual(rig.server.pages(), [0, 1, 2, 3, 4, 5])
        XCTAssertEqual(rig.list.testEngine.cachedPages, [0, 1, 2, 3, 4, 5])
    }

    // MARK: Eviction

    func testTheCacheKeepsAtMostMaxCachedPagesAndEvictsTheLeastRecentlyTouched() throws {
        let rig = try LazyRig(rows: 2000, maxCachedPages: 4)
        for page in [0, 5, 10, 15, 20, 25] {
            rig.read(page * 50)
            XCTAssertLessThanOrEqual(rig.list.testEngine.cachedPages.count, 4)
        }
        // The last read asked for page 25 and its neighbours: they are the newest and stay.
        XCTAssertTrue(Set([24, 25, 26]).isSubset(of: rig.list.testEngine.cachedPages))
        XCTAssertEqual(rig.list[1250], 1250)
    }

    func testAnEvictedPageIsRequestedAgainWhenItIsRead() throws {
        let rig = try LazyRig(rows: 2000, maxCachedPages: 3)
        rig.read(0)
        rig.read(1000)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [19, 20, 21])
        rig.server.forgetCalls()
        XCTAssertNil(rig.list[0], "page 0 left the cache")
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [0, 1], "asked for again")
        XCTAssertEqual(rig.list[0], 0)
    }

    func testReadingKeepsAPageFromBeingEvictedBeforeOlderOnes() throws {
        let rig = try LazyRig(rows: 2000, maxCachedPages: 4)
        rig.read(0)             // pages 0, 1
        rig.read(500)           // pages 9, 10, 11  (cache bound 4: the oldest go)
        XCTAssertEqual(rig.list.testEngine.cachedPages.count, 4)
        XCTAssertTrue(rig.list.testEngine.cachedPages.contains(10))
        _ = rig.list[500]       // touch page 10 and its neighbours again
        rig.read(800)           // pages 15, 16, 17
        XCTAssertEqual(rig.list.testEngine.cachedPages.count, 4)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [10, 15, 16, 17], "page 10 was read again, so it outlived the older ones")
    }

    func testLoweringTheBoundEvictsAtOnce() throws {
        let rig = try LazyRig(rows: 2000)
        rig.prefetchAndRun(0 ..< 500)
        XCTAssertEqual(rig.list.testEngine.cachedPages.count, 10)
        rig.list.maxCachedPages = 3
        XCTAssertEqual(rig.list.testEngine.cachedPages, [7, 8, 9])
        rig.list.maxCachedPages = 0
        XCTAssertEqual(rig.list.maxCachedPages, 1, "at least one page")
        XCTAssertEqual(rig.list.testEngine.cachedPages.count, 1)
    }

    func testAWholeIterationAsksForNoMoreThanTheCacheHolds() throws {
        let rig = try LazyRig(rows: 50_000, maxCachedPages: 8)
        var seen = 0
        for _ in rig.list {
            seen += 1
        }
        XCTAssertEqual(seen, 50_000)
        rig.turns.runUntilQuiet()
        XCTAssertLessThanOrEqual(rig.server.offsets.count, 8, "the most recently read pages only")
        XCTAssertLessThanOrEqual(rig.list.testEngine.cachedPages.count, 8)
    }

    // MARK: Page size

    func testPageSizeSetsTheLimitAndTheBoundariesAndDropsTheCache() throws {
        let rig = try LazyRig(rows: 100)
        rig.read(5)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [0, 1])
        rig.list.pageSize = 10
        XCTAssertEqual(rig.list.pageSize, 10)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [])
        rig.server.forgetCalls()
        rig.read(55)
        XCTAssertEqual(rig.server.offsets, [40, 50, 60])
        XCTAssertEqual(rig.server.limits, [10, 10, 10])
        XCTAssertEqual(rig.list[55], 55)
    }

    func testPageSizeIsClamped() throws {
        let rig = try LazyRig(rows: 10)
        rig.list.pageSize = 0
        XCTAssertEqual(rig.list.pageSize, 1)
        rig.list.pageSize = -7
        XCTAssertEqual(rig.list.pageSize, 1)
        rig.list.pageSize = Int.max
        XCTAssertEqual(rig.list.pageSize, 4096, "the core cuts a page call at 4,096 rows: a larger page would never arrive whole")
        rig.list.pageSize = 1
        rig.read(3)
        XCTAssertEqual(rig.list[3], 3)
        XCTAssertEqual(rig.server.limits, [1, 1, 1])
    }

    // MARK: Invalidation

    func testAnInvalidationTakesTheNewLengthAtOnceAndKeepsStaleRowsVisible() throws {
        let rig = try LazyRig(rows: 120)
        rig.read(10)
        XCTAssertEqual(rig.list[10], 10)
        rig.server.forgetCalls()
        try rig.change { $0[10] = 1010; $0.append(contentsOf: [500, 501, 502]) }
        XCTAssertEqual(rig.list.count, 123, "the length is taken without a round trip")
        XCTAssertEqual(rig.list[10], 10, "the stale row stays while the page is asked for again")
        XCTAssertEqual(rig.server.offsets, [], "nothing is sent before the turn")
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list[10], 1010)
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(0), 2)
    }

    func testAnInvalidationAsksForTheWindowOnlyNotTheList() throws {
        let rig = try LazyRig(rows: 5000)          // 100 pages
        rig.read(500, 600)                          // pages 9...13 (neighbours included)
        XCTAssertEqual(rig.list.testEngine.windowPages, [9, 10, 11, 12, 13])
        rig.server.forgetCalls()
        try rig.change { $0[600] = -600 }
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [9, 10, 11, 12, 13], "O(window), never O(list)")
        XCTAssertEqual(rig.list[600], -600)
    }

    func testTheWindowIsWhatWasReadSinceThePreviousInvalidation() throws {
        let rig = try LazyRig(rows: 5000)
        rig.read(0)                                 // pages 0, 1
        try rig.change { $0[0] = 7 }
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [0, 1, 0, 1])
        rig.server.forgetCalls()
        rig.read(2000)                              // pages 39, 40, 41: the new window
        rig.server.forgetCalls()
        try rig.change { $0[2000] = 8 }
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [39, 40, 41], "the pages read before the previous change are not")
        XCTAssertEqual(rig.list[2000], 8)
    }

    func testAStalePageIsAskedForAgainWhenItIsRead() throws {
        let rig = try LazyRig(rows: 5000)
        rig.read(0)
        rig.read(2000)
        try rig.change { $0[1] = 1 }                // every page read so far is the window of this one
        rig.turns.runUntilQuiet()
        rig.read(2000)                              // from here on the window is pages 39...41
        try rig.change { $0[0] = 99 }
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(0), 2, "page 0 was not in the window")
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(40), 3)
        rig.server.forgetCalls()
        XCTAssertEqual(rig.list[0], 0, "stale rows are shown meanwhile")
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [0, 1])
        XCTAssertEqual(rig.list[0], 99)
    }

    func testAShorterListDropsThePagesBeyondItsEnd() throws {
        let rig = try LazyRig(rows: 500)
        rig.read(300, 0)
        XCTAssertTrue(rig.list.testEngine.cachedPages.contains(6))
        try rig.change { $0.removeLast(380) }       // 120 rows left
        XCTAssertEqual(rig.list.count, 120)
        XCTAssertNil(rig.list[300])
        XCTAssertFalse(rig.list.testEngine.cachedPages.contains(6))
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list.testEngine.cachedPages.filter { $0 > 2 }, [])
        rig.read(119)
        XCTAssertEqual(rig.list[119], 119)
    }

    func testAGrowingListRefreshesItsPartialLastPage() throws {
        let rig = try LazyRig(rows: 70)
        rig.read(60)
        XCTAssertEqual(rig.list[69], 69)
        XCTAssertNil(rig.list[70])
        try rig.change { $0.append(contentsOf: (70 ..< 130).map { Int32($0) }) }
        XCTAssertEqual(rig.list.count, 130)
        XCTAssertNil(rig.list[75], "page 1 is stale and short")
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list[75], 75)
        XCTAssertEqual(rig.list[129], 129)
    }

    func testAnInvalidationThatIsNotNewerChangesNothing() throws {
        let rig = try LazyRig(rows: 100)
        rig.read(0)
        rig.server.forgetCalls()
        let before = rig.turns.scheduled
        try rig.applyInvalidated(UndraLazyInvalidated(len: 5, version: 1).undraEncoded())
        try rig.applyInvalidated(UndraLazyInvalidated(len: 5, version: 0).undraEncoded())
        XCTAssertEqual(rig.list.count, 100)
        XCTAssertEqual(rig.turns.scheduled, before)
    }

    func testAnInvalidationBeforeTheFirstValueIsIgnored() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let list = UndraLazyList<Int32>(core: core)
        var reader = UndraReader(UndraLazyInvalidated(len: 10, version: 3).undraEncoded())
        try list.applyInvalidated(&reader)
        XCTAssertEqual(list.count, 0)
        XCTAssertNil(list[0])
    }

    // MARK: Restart

    func testANewHandleRestartsTheListAndDropsTheCache() throws {
        let rig = try LazyRig(rows: 300)
        rig.read(0, 100)
        XCTAssertFalse(rig.list.testEngine.cachedPages.isEmpty)
        let restored = UndraHandle(index: 9, generation: 2)
        rig.server.mutate { $0.append(contentsOf: [1, 2, 3]) }
        try rig.applyValue(rig.server.value(handle: restored))
        XCTAssertEqual(rig.list.count, 303)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [])
        XCTAssertNil(rig.list[0], "the rows of the old page server are gone")
        rig.server.forgetCalls()
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.targets, [restored, restored], "asked of the new page server")
    }

    func testTheSameHandleWithANewerVersionIsARefreshAndOlderOneIsIgnored() throws {
        let rig = try LazyRig(rows: 100)
        rig.read(0)
        rig.server.forgetCalls()
        rig.server.mutate { $0[1] = 11 }
        try rig.applyValue(rig.server.value())
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list[1], 11, "re-observing a signal re-sends its value: the window is paged again")
        XCTAssertEqual(rig.server.pages(), [0, 1])
        rig.server.forgetCalls()
        try rig.applyValue(UndraLazyValue(handle: rig.server.handle, len: 3, version: 1).undraEncoded())
        XCTAssertEqual(rig.list.count, 100, "an older value of the same page server is ignored")
    }

    func testReleasingTheListReleasesNothingOfTheCore() throws {
        let transport = FakeTransport()
        let server = LazyServer(rows: 100)
        server.install(on: transport)
        let core = try makeCore(transport)
        weak var weakList: UndraLazyList<Int32>?
        do {
            let list = UndraLazyList<Int32>(core: core)
            weakList = list
            var reader = UndraReader(server.value())
            try list.applyFull(&reader)
            _ = list[0]
        }
        XCTAssertNil(weakList, "nothing retains the list but its owner")
        XCTAssertEqual(transport.releases, [], "the page server belongs to the core, whose store releases it")
    }

    // MARK: Failures of the call

    func testAFailedPageCallIsReportedAndTheRowIsAskedForAgainOnTheNextRead() throws {
        let rig = try LazyRig(rows: 100)
        rig.transport.onCallSync = { call in
            return Wire.Reply(callId: call.callId, status: .badRequest, body: ArraySlice(lazyBadRequestBody("the page server is gone"))).encode()
        }
        XCTAssertNil(rig.list[0])
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.errors.count, 2, "pages 0 and 1 failed")
        XCTAssertTrue(rig.errors.all[0].description.contains("UndraLazyList.page(0)"))
        XCTAssertTrue(rig.errors.all[0].description.contains("the page server is gone"))
        XCTAssertEqual(rig.list.testEngine.cachedPages, [])
        rig.server.install(on: rig.transport)
        XCTAssertNil(rig.list[0])
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list[0], 0, "the next read tried again")
    }

    func testAShutDownCoreIsReportedNotThrown() throws {
        let rig = try LazyRig(rows: 100)
        rig.core.shutdown()
        XCTAssertNil(rig.list[0])
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.errors.count, 2)
        XCTAssertEqual(rig.list.count, 100)
    }

    // MARK: Asynchronous transports

    func testAnAsynchronousTransportAsksAsynchronouslyAndKeepsWhatIsInFlight() async throws {
        let rig = try LazyRig(rows: 500, direct: false, hold: true)
        XCTAssertNil(rig.list[260])
        rig.turns.run()
        let sent = await waitUntil { rig.server.offsets.count == 3 }
        XCTAssertTrue(sent)
        XCTAssertEqual(rig.server.pages().sorted(), [4, 5, 6], "the batch is on its way; the order the calls leave in is the runtime's")
        XCTAssertEqual(rig.list.testEngine.inFlightPages, [4, 5, 6])
        XCTAssertNil(rig.list[260], "the reply has not come")
        // Reads of the same rows while the pages are on their way ask for nothing.
        rig.turns.runUntilQuiet()
        try await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertEqual(rig.server.offsets.count, 3)
        rig.server.release(on: rig.transport)
        let arrived = await waitUntil { rig.list[260] != nil }
        XCTAssertTrue(arrived)
        XCTAssertEqual(rig.list[260], 260)
        XCTAssertEqual(rig.list[300], 300)
        XCTAssertEqual(rig.list.testEngine.inFlightPages, [])
        XCTAssertEqual(rig.server.offsets.count, 3)
    }

    func testAnAsynchronousTransportNeverBlocksTheCallerOnACall() async throws {
        // `callSync` over a transport without an inline path would block for the call's reply; the list must use `call`.
        let rig = try LazyRig(rows: 100, direct: false, hold: true)
        XCTAssertNil(rig.list[0])
        rig.turns.run()                    // would hang here (30 s) if the engine used callSync
        let sent = await waitUntil { rig.server.offsets.count == 2 }
        XCTAssertTrue(sent)
        XCTAssertEqual(rig.core.stats().hostPendingCalls, 2)
        rig.server.release(on: rig.transport)
        let arrived = await waitUntil { rig.list[0] != nil }
        XCTAssertTrue(arrived)
    }

    func testAReplyOlderThanTheListIsDroppedAndAskedForAgain() async throws {
        let rig = try LazyRig(rows: 100, direct: false, hold: true)
        XCTAssertNil(rig.list[5])
        rig.turns.run()
        let sent = await waitUntil { rig.server.heldCount == 2 }
        XCTAssertTrue(sent)                           // pages 0 and 1, read at version 1
        try rig.change { $0[5] = 555 }                // version 2, while they are on their way
        rig.server.release(on: rig.transport)         // the version 1 replies come
        let asked = await waitUntil { rig.turns.pending.count == 1 }
        XCTAssertTrue(asked, "the stale pages are asked for again")
        XCTAssertNil(rig.list[5], "a stale reply is not shown")
        XCTAssertEqual(rig.list.testEngine.cachedPages, [])
        rig.turns.run()
        let again = await waitUntil { rig.server.heldCount == 2 }
        XCTAssertTrue(again)
        rig.server.release(on: rig.transport)
        let fresh = await waitUntil { rig.list[5] == 555 }
        XCTAssertTrue(fresh)
        XCTAssertEqual(rig.server.pages().sorted(), [0, 0, 1, 1])
        XCTAssertEqual(rig.errors.count, 0)
    }

    func testAReplyNewerThanTheListRaisesItsVersionAndLength() throws {
        let rig = try LazyRig(rows: 200)
        rig.read(5)                                   // pages 0 and 1 at version 1
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(0), 1)
        // The core changed (200 -> 250 rows); its invalidation has not reached the host yet.
        rig.server.mutate { $0.append(contentsOf: (200 ..< 250).map { Int32($0) }) }
        rig.server.forgetCalls()
        rig.read(180)                                 // pages 2 and 3: the replies are at version 2
        XCTAssertEqual(rig.list.count, 250, "a newer reply carries the new length")
        XCTAssertEqual(rig.list.testEngine.version, 2)
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(2), 2)
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(3), 2)
        XCTAssertEqual(rig.server.pages(), [2, 3, 0, 1], "the older pages of the window are asked for again")
        XCTAssertEqual(rig.list.testEngine.versionOfCachedPage(0), 2)
        // The invalidation that follows says nothing new.
        rig.server.forgetCalls()
        let scheduled = rig.turns.scheduled
        try rig.applyInvalidated(rig.server.invalidated())
        XCTAssertEqual(rig.turns.scheduled, scheduled)
        XCTAssertEqual(rig.server.offsets, [])
        XCTAssertEqual(rig.list.count, 250)
    }

    func testAnInvalidationInsideACallsDrainMakesItsReplyStaleAndTheRowsArrive() throws {
        // In process a page call is synchronous and applies the change-sets that arrived before it returns: an invalidation lands
        // in the middle of the batch, and the reply read before it is stale.
        let transport = FakeTransport()
        let server = LazyServer(rows: 100)
        server.install(on: transport)
        let core = try makeCore(transport)
        let turns = LazyTurns()
        let store = LazyLibraryStore(core: core, handle: UndraHandle(index: 1, generation: 1), schedule: { turns.schedule($0) })
        transport.deliverChangeSet(Wire.ChangeSet(txnId: 1, entries: [
            Wire.ChangeEntry(handle: store.handle, signalId: 3, op: .fullValue, value: ArraySlice(server.value())),
        ]))
        core.mirror.flush()
        XCTAssertEqual(store.books.count, 100)
        server.afterReply { [weak transport] in
            server.afterReply(nil)
            server.mutate { $0[2] = 222 }
            transport?.deliverChangeSet(Wire.ChangeSet(txnId: 2, entries: [
                Wire.ChangeEntry(handle: store.handle, signalId: 3, op: .lazyListInvalidated, value: ArraySlice(server.invalidated())),
            ]))
        }
        XCTAssertNil(store.books[2])
        turns.runUntilQuiet()
        XCTAssertEqual(store.books[2], 222)
        XCTAssertEqual(store.books[60], 60)
        XCTAssertEqual(server.pages(), [0, 1, 0], "page 0 was read before the invalidation: dropped and asked for again; page 1 after it")
    }

    func testTheOldRepliesOfAReplacedPageServerAreDropped() async throws {
        let rig = try LazyRig(rows: 100, direct: false, hold: true)
        XCTAssertNil(rig.list[0])
        rig.turns.run()
        let sent = await waitUntil { rig.server.heldCount == 2 }
        XCTAssertTrue(sent)
        let replacement = UndraHandle(index: 8, generation: 3)
        rig.server.mutate { $0 = $0.map { $0 + 1000 } }
        try rig.applyValue(rig.server.value(handle: replacement))
        rig.server.release(on: rig.transport)           // the old page server's replies
        try await Task.sleep(nanoseconds: 30_000_000)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [], "nothing of the old page server is cached")
        XCTAssertNil(rig.list[0])
        rig.turns.run()
        let asked = await waitUntil { rig.server.heldCount == 2 }
        XCTAssertTrue(asked)
        rig.server.release(on: rig.transport)
        let arrived = await waitUntil { rig.list[0] == 1000 }
        XCTAssertTrue(arrived)
        XCTAssertEqual(Array(rig.server.targets.suffix(2)), [replacement, replacement])
    }

    func testChangingThePageSizeDropsWhatWasAskedAtTheOldSize() async throws {
        let rig = try LazyRig(rows: 100, direct: false, hold: true)
        XCTAssertNil(rig.list[0])
        rig.turns.run()
        let sent = await waitUntil { rig.server.heldCount == 2 }
        XCTAssertTrue(sent)
        rig.list.pageSize = 10
        rig.server.release(on: rig.transport)           // two pages of 50 rows, for boundaries that no longer exist
        try await Task.sleep(nanoseconds: 30_000_000)
        XCTAssertEqual(rig.list.testEngine.cachedPages, [])
        XCTAssertEqual(rig.errors.count, 0)
        XCTAssertNil(rig.list[0])
        rig.turns.run()
        let asked = await waitUntil { rig.server.heldCount == 2 }
        XCTAssertTrue(asked)
        rig.server.release(on: rig.transport)
        let arrived = await waitUntil { rig.list[0] == 0 }
        XCTAssertTrue(arrived)
        XCTAssertEqual(rig.server.limits.suffix(2).map { Int($0) }, [10, 10])
    }

    func testStaleRepliesAreAskedForAgainAFewTimesThenReported() throws {
        let rig = try LazyRig(rows: 100)
        rig.server.mutate { _ in }
        try rig.applyInvalidated(rig.server.invalidated())      // the list is at version 2, the server answers at version 1
        rig.server.respond { offset, _ in
            return lazyPageBody(version: 1, total: 100, count: UInt32(min(50, 100 - Int(offset))), rows: (0 ..< min(50, 100 - Int(offset))).map { Int32($0) })
        }
        rig.list.prefetch(0 ..< 1)
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.server.pages(), [0, 0, 0, 0], "the first ask and three more")
        XCTAssertEqual(rig.errors.count, 1)
        XCTAssertTrue(rig.errors.all[0].description.contains("keeps answering page 0"))
        XCTAssertNil(rig.list[0])
    }

    // MARK: The wire values

    func testTheSignalsValueThatDoesNotDecodeLeavesTheListAsItWas() throws {
        let rig = try LazyRig(rows: 100)
        let good = rig.server.value()
        for cut in 0 ..< good.count {
            var reader = UndraReader(Array(good.prefix(cut)))
            XCTAssertThrowsError(try rig.list.applyFull(&reader), "a value cut at \(cut)")
        }
        var trailing = UndraReader(good + [0])
        XCTAssertThrowsError(try rig.list.applyFull(&trailing)) { error in
            XCTAssertEqual(error as? WireError, .trailingBytes(count: 1))
        }
        XCTAssertEqual(rig.list.count, 100)
        XCTAssertEqual(rig.list.testEngine.version, 1)
    }

    func testTheNullHandleIsRefused() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let list = UndraLazyList<Int32>(core: core)
        var reader = UndraReader(UndraLazyValue(handle: .null, len: 5, version: 1).undraEncoded())
        XCTAssertThrowsError(try list.applyFull(&reader)) { error in
            XCTAssertEqual(error as? UndraLazyListError, .nullHandle)
        }
        XCTAssertEqual(list.count, 0)
    }

    func testAnInvalidationThatDoesNotDecodeThrows() throws {
        let rig = try LazyRig(rows: 100)
        let good = UndraLazyInvalidated(len: 5, version: 9).undraEncoded()
        for cut in 0 ..< good.count {
            var reader = UndraReader(Array(good.prefix(cut)))
            XCTAssertThrowsError(try rig.list.applyInvalidated(&reader))
        }
        var trailing = UndraReader(good + [1, 2])
        XCTAssertThrowsError(try rig.list.applyInvalidated(&trailing))
        XCTAssertEqual(rig.list.count, 100)
    }

    // MARK: Cost

    func testReadingCachedRowsIsCheap() throws {
        // The hot path of a view: rows of cached pages, read over and over. What it asserts is that no read costs a call (the count
        // of calls below); how fast the machine does 100,000 reads is not a property of the code (the budget is in `bench/`, R9).
        let rig = try LazyRig(rows: 5000)
        rig.list.prefetch(0 ..< 1200)
        rig.turns.runUntilQuiet()
        XCTAssertEqual(rig.list.testEngine.cachedPages.count, 24)
        let calls = rig.server.offsets.count
        var sum = 0
        for round in 0 ..< 200 {
            for index in 0 ..< 500 {
                sum += Int(rig.list[(round * 7 + index) % 1200] ?? 0)
            }
        }
        XCTAssertGreaterThan(sum, 0)
        XCTAssertEqual(rig.server.offsets.count, calls, "no read of a cached row asks for anything")
    }

    // MARK: The default turn

    func testTheDefaultSchedulerSendsTheRequestsAfterTheTurnAsOneBatch() async throws {
        let transport = FakeTransport()
        let server = LazyServer(rows: 500)
        server.install(on: transport)
        let core = try makeCore(transport)
        let list = UndraLazyList<Int32>(core: core)
        var reader = UndraReader(server.value())
        try list.applyFull(&reader)
        _ = (list[10], list[11], list[60], list[0])
        XCTAssertEqual(server.offsets, [], "nothing is sent while the turn that read the rows runs")
        let arrived = await waitUntil { list[10] != nil }
        XCTAssertTrue(arrived)
        XCTAssertEqual(server.pages(), [0, 1, 2])
        XCTAssertEqual(list[60], 60)
    }
}

extension LazyRig {
    /// Prefetches a range and runs the turn.
    func prefetchAndRun(_ range: Range<Int>) {
        list.prefetch(range)
        turns.runUntilQuiet()
    }
}

/// The body of a bad-request reply: the reason.
func lazyBadRequestBody(_ reason: String) -> [UInt8] {
    var writer = UndraWriter()
    writer.writeString(reason)
    return writer.finish()
}
