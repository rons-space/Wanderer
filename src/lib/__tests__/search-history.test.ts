import { describe, expect, it } from "vitest";
import {
    MAX_HISTORY_ITEMS,
    SEARCH_HISTORY_KEY,
    addToHistory,
    buildSearchFilters,
    readSearchHistory,
    removeFromHistory,
    writeSearchHistory,
} from "@/lib/search-history";

/** Just enough of Storage to drive the two functions that touch it. */
const fakeStorage = (initial?: string) => {
    let value = initial;
    return {
        getItem: (key: string) => (key === SEARCH_HISTORY_KEY ? (value ?? null) : null),
        setItem: (key: string, next: string) => {
            if (key === SEARCH_HISTORY_KEY) value = next;
        },
        read: () => value,
    };
};

describe("readSearchHistory", () => {
    it("reads back what was written", () => {
        const storage = fakeStorage();
        writeSearchHistory(["beach", "dog"], storage);
        expect(readSearchHistory(storage)).toEqual(["beach", "dog"]);
    });

    it("treats missing storage as no history", () => {
        expect(readSearchHistory(fakeStorage())).toEqual([]);
    });

    it("survives a value that is not JSON at all", () => {
        expect(readSearchHistory(fakeStorage("{not json"))).toEqual([]);
    });

    it("survives JSON of the wrong shape", () => {
        expect(readSearchHistory(fakeStorage('{"queries":["beach"]}'))).toEqual([]);
        expect(readSearchHistory(fakeStorage("42"))).toEqual([]);
    });

    it("drops non-string entries rather than rendering them", () => {
        expect(readSearchHistory(fakeStorage('["beach", 7, null, {"a":1}, "dog"]'))).toEqual([
            "beach",
            "dog",
        ]);
    });
});

describe("writeSearchHistory", () => {
    it("never stores more than the cap, however long the list is", () => {
        const storage = fakeStorage();
        const long = Array.from({ length: MAX_HISTORY_ITEMS + 5 }, (_, index) => `q${index}`);
        writeSearchHistory(long, storage);
        expect(readSearchHistory(storage)).toHaveLength(MAX_HISTORY_ITEMS);
        expect(readSearchHistory(storage)[0]).toBe("q0");
    });
});

describe("addToHistory", () => {
    it("puts the newest query first", () => {
        expect(addToHistory(["dog"], "beach")).toEqual(["beach", "dog"]);
    });

    it("moves a repeated query to the front instead of duplicating it", () => {
        expect(addToHistory(["dog", "beach", "cat"], "beach")).toEqual(["beach", "dog", "cat"]);
    });

    it("ignores a blank query", () => {
        const history = ["dog"];
        expect(addToHistory(history, "")).toBe(history);
        expect(addToHistory(history, "   ")).toBe(history);
    });

    it("keeps the list at the cap by dropping the oldest", () => {
        const full = Array.from({ length: MAX_HISTORY_ITEMS }, (_, index) => `q${index}`);
        const next = addToHistory(full, "newest");
        expect(next).toHaveLength(MAX_HISTORY_ITEMS);
        expect(next[0]).toBe("newest");
        expect(next).not.toContain(`q${MAX_HISTORY_ITEMS - 1}`);
    });

    it("does not mutate the list it was given", () => {
        const history = ["dog"];
        addToHistory(history, "beach");
        expect(history).toEqual(["dog"]);
    });
});

describe("removeFromHistory", () => {
    it("removes every copy of the query and leaves the rest in order", () => {
        expect(removeFromHistory(["dog", "beach", "dog"], "dog")).toEqual(["beach"]);
    });

    it("is a no-op for a query that is not there", () => {
        expect(removeFromHistory(["dog"], "cat")).toEqual(["dog"]);
    });
});

describe("buildSearchFilters", () => {
    const neutral = { favoritesOnly: false, minRating: "0", cameraMake: "", hasLocation: "any" };

    it("leaves every neutral control undefined rather than falsy", () => {
        const filters = buildSearchFilters(neutral);
        expect(filters).toEqual({
            favorites_only: false,
            min_rating: undefined,
            camera_make: undefined,
            has_location: undefined,
        });
    });

    it("passes a real rating through as a number", () => {
        expect(buildSearchFilters({ ...neutral, minRating: "4" }).min_rating).toBe(4);
    });

    it("ignores an unparseable rating", () => {
        expect(buildSearchFilters({ ...neutral, minRating: "" }).min_rating).toBeUndefined();
    });

    it("trims the camera make and drops it when only whitespace is left", () => {
        expect(buildSearchFilters({ ...neutral, cameraMake: "  Canon " }).camera_make).toBe("Canon");
        expect(buildSearchFilters({ ...neutral, cameraMake: "   " }).camera_make).toBeUndefined();
    });

    it("distinguishes the three location states", () => {
        expect(buildSearchFilters({ ...neutral, hasLocation: "any" }).has_location).toBeUndefined();
        expect(buildSearchFilters({ ...neutral, hasLocation: "yes" }).has_location).toBe(true);
        expect(buildSearchFilters({ ...neutral, hasLocation: "no" }).has_location).toBe(false);
    });
});
