import Foundation
import PlaygroundCore
@testable import UndraRuntime
import XCTest

/// Runs `body` on a background thread and blocks the calling thread until it has finished. A caller on the main actor holds
/// the frame meanwhile: the mirror's drain (a display link tick, or the main actor's next turn) cannot run until it returns.
private func onBackgroundThread(_ body: @escaping @Sendable () -> Void) {
    let group = DispatchGroup()
    group.enter()
    DispatchQueue.global().async {
        body()
        group.leave()
    }
    group.wait()
}

/// Where the library a callback made ends up (S32 step 4).
@MainActor
private final class MadeLibrary {
    var library: Library?
    var failure: String?
}

extension ContractScenarios {
    // MARK: S32

    /// ADR-043: a `Lazy<T>` list is not sent whole, pages are requested once with one page of prefetch each side, a change is
    /// one 12-byte entry and the host re-pages its window, a drain folds without losing the page server, a view pages through
    /// the derived index, and an infinite query grows a page at a time. The page calls are counted where the host sends them
    /// (`UndraStats.hostPageCalls`: every target-3 call of `UndraCore.call` and `callSync`).
    func testS32_pagedQueriesAndLazyLists() async {
        await scenario("S32", "paged queries and lazy lists") {
            let core = try self.core
            let ids = UndraIds.Objects.Library.self
            let pageCalls = { core.stats().hostPageCalls }
            let one = encoded { (w: inout UndraWriter) in w.writeU32(1) }

            // 1. A lazy list is not sent whole.
            let raw1 = try RawStore(core: core, type: ids.typeId, method: ids.new)
            defer { raw1.close() }
            raw1.observe()
            let firstBooks = raw1.entries(of: 0)
            try checkEqual(firstBooks.count, 1, "initial entries for books")
            try checkEqual(firstBooks[0].op, .fullValue, "form of the initial books entry")
            try checkEqual(firstBooks[0].value.count, 20, "the bytes of the books entry (a LazyValue, not 10,000 rows)")
            let firstValue = try firstBooks[0].decode(UndraLazyValue.self)
            try checkEqual(firstValue.len, 10_000, "the length in the LazyValue")
            let firstBytes = raw1.entries.reduce(0) { $0 + $1.value.count }
            try check(firstBytes < 2_048, "the store's first change-set carries \(firstBytes) bytes (expected < 2048)")

            let library = try Library(ctx: core)
            defer { library.close() }
            try checkEqual(library.books.count, 10_000, "books.count after the first drain")
            try check(library.books[0] == nil, "books[0] is nothing until its page arrives")
            try await waitUntil("books[0]") { library.books[0] != nil }
            try checkEqual(library.books[0]?.id, 1, "books[0].id")
            try await waitUntil("books[9999]") { library.books[9_999] != nil }
            try checkEqual(library.books[9_999]?.id, 10_000, "books[9999].id (the last page)")
            let beforeOutside = pageCalls()
            try check(library.books[10_000] == nil && library.books[-1] == nil && library.books[Int.max] == nil, "an index outside 0..<count is nothing")
            try await quietFor(milliseconds: 100)
            try checkEqual(pageCalls(), beforeOutside, "page calls after reads outside the list")

            // 2. A page is requested once, with one page of prefetch each side.
            let library2 = try Library(ctx: core)
            defer { library2.close() }
            let before2 = pageCalls()
            try check(library2.books[120] == nil, "books[120] before its page arrives")
            try await waitUntil("books[120]") { library2.books[120] != nil }
            try checkEqual(pageCalls() - before2, 3, "page calls for the first read of row 120")
            try checkEqual(library2.books.testEngine.cachedPages, [1, 2, 3], "the pages that were requested (2, then 1 and 3)")
            let after2 = pageCalls()
            for index in 100 ..< 150 {
                try checkEqual(library2.books[index]?.id, UInt32(index + 1), "books[\(index)].id from the cached page")
            }
            try await quietFor(milliseconds: 100)
            try checkEqual(pageCalls(), after2, "page calls for the rows of a cached page")
            let server = try require(library2.books.testEngine.handle, "the page server of books")
            var reply = UndraReader(try core.callSync(.lazyListPage(handle: server, offset: 100, limit: 1), method: 0, args: []))
            let header = try UndraLazyPageHeader.undraDecode(&reply)
            try checkEqual(library2.books.testEngine.version, header.version, "the list's version and the version in a page reply")

            // 3. A change is one 12-byte entry and the host re-pages only its window.
            let raw3 = try RawStore(core: core, type: ids.typeId, method: ids.new)
            defer { raw3.close() }
            raw3.observe()
            let version3 = try raw3.entries(of: 0)[0].decode(UndraLazyValue.self).version
            raw3.clear()
            let received3 = core.mirror.stats().changeSetsReceived
            try raw3.callSync(ids.addRows, one)
            try await waitUntil("the invalidation of add_rows(1)") { !raw3.entries(of: 0).isEmpty }
            let invalidations = raw3.entries(of: 0)
            try checkEqual(invalidations.count, 1, "books entries for add_rows(1)")
            try checkEqual(invalidations[0].op, .lazyListInvalidated, "form of the books entry")
            try checkEqual(invalidations[0].value.count, 12, "the bytes of the books entry")
            let invalidated = try invalidations[0].decode(UndraLazyInvalidated.self)
            try checkEqual(invalidated.len, 10_001, "the length after add_rows(1)")
            try check(invalidated.version > version3, "the version rose: \(invalidated.version) after \(version3)")
            try checkEqual(core.mirror.stats().changeSetsReceived - received3, 1, "change-sets for add_rows(1)")

            let beforeChange = pageCalls()
            library2.addRows(count: 1)
            try checkEqual(library2.books.count, 10_001, "count after add_rows(1)")
            try check(library2.books[120] != nil, "a row already on screen stays readable while its page is asked again")
            try checkEqual(pageCalls(), beforeChange, "nothing is asked from inside the call")
            try await waitUntil("the window to be asked again") { pageCalls() - beforeChange >= 3 }
            try await quietFor(milliseconds: 100)
            try checkEqual(pageCalls() - beforeChange, 3, "page calls for the window of three pages (not 200)")
            try await waitUntil("the pages at the new version") {
                library2.books.testEngine.versionOfCachedPage(2) == library2.books.testEngine.version
            }
            try library2.rename(index: 121, label: "x")
            try await waitUntil("row 121 renamed") { library2.books[121]?.label == "x" }
            try checkEqual(library2.books[121]?.version, 1, "books[121].version after rename")
            try library2.removeAt(index: 0)
            try checkEqual(library2.books.count, 10_000, "count after remove_at(0)")
            try await waitUntil("books[0] after remove_at(0)") { library2.books[0]?.id == 2 }

            // 4. A drain folds without losing the page server.
            let raw4 = try RawStore(core: core, type: ids.typeId, method: ids.new)
            defer { raw4.close() }
            raw4.observe()
            let value4 = try raw4.entries(of: 0)[0].decode(UndraLazyValue.self)
            raw4.clear()
            let received4 = core.mirror.stats().changeSetsReceived
            // The calls run on another thread while this one holds the main actor, so no frame can drain between them.
            let failures = Locked<[String]>([])
            let target4 = raw4.handle
            onBackgroundThread {
                for _ in 0 ..< 3 {
                    do {
                        _ = try core.callSync(.objectMethod(handle: target4, methodId: ids.addRows), method: ids.addRows, args: one)
                    } catch {
                        failures.withLock { $0.append("\(error)") }
                    }
                }
            }
            try checkEqual(failures.snapshot, [], "failures of add_rows(1) made while the frame was held")
            try checkEqual(core.mirror.stats().changeSetsReceived - received4, 3, "change-sets queued while the frame was held")
            try check(raw4.entries(of: 0).isEmpty, "nothing was delivered while the frame was held")
            core.mirror.flush()
            let folded = raw4.entries(of: 0)
            try checkEqual(folded.count, 1, "books deliveries after the frame was released")
            try checkEqual(folded[0].op, .lazyListInvalidated, "form of the delivery")
            let foldedValue = try folded[0].decode(UndraLazyInvalidated.self)
            try checkEqual(foldedValue.len, 10_003, "the last length")
            var latest = UndraReader(try core.callSync(.lazyListPage(handle: value4.handle, offset: 0, limit: 1), method: 0, args: []))
            try checkEqual(foldedValue.version, try UndraLazyPageHeader.undraDecode(&latest).version, "the last version")

            // A second library whose add_rows(1) runs before its first drain: it is made, and changed, from inside a drain
            // (where the mirror applies what a call causes in the drain's next round), so the drain holds the value that names
            // the page server and then the invalidation.
            let trigger = try RawStore(core: core, type: ids.typeId, method: ids.new)
            defer { trigger.close() }
            trigger.observe()
            trigger.clear()
            let made = MadeLibrary()
            trigger.onEntry = { _ in
                guard made.library == nil, made.failure == nil else {
                    return
                }
                do {
                    let created = try Library(ctx: core)
                    created.addRows(count: 1)
                    made.library = created
                } catch {
                    made.failure = "\(error)"
                }
            }
            try trigger.callSync(ids.addRows, one)
            trigger.onEntry = nil
            try check(made.failure == nil, "the library made inside the drain: \(made.failure ?? "")")
            let second = try require(made.library, "the library made inside the drain")
            defer { second.close() }
            try checkEqual(second.books.count, 10_001, "count of a library changed before its first drain")
            try await waitUntil("books[0] of that library") { second.books[0] != nil }
            try checkEqual(second.books[0]?.id, 1, "books[0].id of that library")

            // 5. A view pages through the derived index.
            let library5 = try Library(ctx: core)
            defer { library5.close() }
            try checkEqual(library5.evens.count, 10, "evens.count")
            try await waitUntil("evens[0] and evens[9]") { library5.evens[0] != nil && library5.evens[9] != nil }
            try checkEqual(library5.evens[0]?.id, 2, "evens[0].id")
            try checkEqual(library5.evens[9]?.id, 20, "evens[9].id")
            library5.addRows(count: 2)
            try await waitUntil("evens.count 11") { library5.evens.count == 11 }
            try await waitUntil("evens[10]") { library5.evens[10] != nil }
            try checkEqual(library5.evens[10]?.id, 10_002, "evens[10].id")
            library5.dropSource(count: 20)
            try await waitUntil("evens.count 1") { library5.evens.count == 1 }

            // 6. An infinite query grows a page at a time.
            let feedIds = UndraIds.Objects.FeedQueryHandle.self
            let feed = try FeedQueryHandle(evenOnly: false, ctx: core)
            defer { feed.close() }
            try await waitUntil("the first page of the feed") { feed.data.count == 50 && feed.hasNextPage }
            let shared = try FeedQueryHandle(evenOnly: false, ctx: core)
            defer { shared.close() }
            try checkEqual(shared.data.count, 50, "a second handle with the same parameter starts with the shared rows")
            try check(!shared.fetching, "...and does not fetch on its own")
            try await quietFor(milliseconds: 150)
            try checkEqual(shared.data.count, 50, "the second handle's rows a moment later")
            let rawFeed = try RawStore(
                core: core, type: feedIds.typeId, method: feedIds.new,
                args: encoded { (w: inout UndraWriter) in false.undraEncode(&w) }
            )
            defer { rawFeed.close() }
            rawFeed.observe()
            try checkEqual(try rawFeed.entries(of: 0).first?.decode([Item].self).count, 50, "rows a third observer starts with")
            rawFeed.clear()
            let nextPage = ChangeRecorder { feed.fetchingNextPage }
            feed.fetchNextPage()
            try await waitUntil("100 rows") { feed.data.count == 100 && !feed.fetchingNextPage }
            nextPage.stop()
            try check(nextPage.values.first == false && nextPage.values.contains(true) && nextPage.values.last == false, "fetchingNextPage went true and then false: \(nextPage.values)")
            // The page reaches the two handles in one transaction, but as one change-set per store, handed over one after the
            // other from the core's thread: a frame can drain the first before the second arrives. So the shared handle is
            // waited for on its own, and then holds the same rows.
            try await waitUntil("the shared handle to follow to 100 rows") { shared.data.count == 100 }
            try checkEqual(shared.data.map(\.id), feed.data.map(\.id), "the shared handle follows: the same rows as the first")
            let patch = rawFeed.entries(of: 0)
            try checkEqual(patch.count, 1, "data entries for the next page")
            try checkEqual(patch[0].op, .keyedPatch, "form of the data entry for the next page")
            try check(patch[0].value.count < 5_000, "the patch is \(patch[0].value.count) bytes (expected < 5,000)")
            let inserts: [PatchOp<Item>] = try patch[0].decodePatch(Item.self)
            try checkEqual(inserts.count, 50, "operations of the next page's patch")
            try check(inserts.allSatisfy { if case .insert = $0 { return true } else { return false } }, "every operation inserts")

            touchFeed(revision: 7, ctx: core)
            rawFeed.clear()
            feed.refetch()
            try await waitUntil("the even rows at version 7") {
                feed.data.count == 100 && feed.data.allSatisfy { $0.version == ($0.id % 2 == 0 ? 7 : 0) }
            }
            try await quietFor(milliseconds: 100)
            let refetched = rawFeed.entries(of: 0)
            try check(!refetched.isEmpty && refetched.allSatisfy { $0.op == .keyedPatch }, "the refetch arrives as keyed patches: \(refetched.map(\.op))")
            var updates = 0
            for entry in refetched {
                let ops: [PatchOp<Item>] = try entry.decodePatch(Item.self)
                try check(ops.allSatisfy { if case .update = $0 { return true } else { return false } }, "the refetch only updates rows")
                updates += ops.count
            }
            try check(updates <= 50, "updates in the refetch's patches: \(updates) (expected at most 50)")

            let evens = try FeedQueryHandle(evenOnly: true, ctx: core)
            defer { evens.close() }
            try await waitUntil("the even-only feed") { evens.data.count == 50 }
            try checkEqual(evens.data.map(\.id), (1 ... 50).map { UInt32($0 * 2) }, "the rows of the even-only feed")
            evens.loadMore(ifNeededFor: evens.data[10])
            try await quietFor(milliseconds: 150)
            try check(!evens.fetchingNextPage && evens.data.count == 50, "loadMore for a row far from the end fetches nothing")
            evens.loadMore(ifNeededFor: evens.data[47])
            try await waitUntil("the next page after loadMore") { evens.data.count == 100 }
        }
    }
}
