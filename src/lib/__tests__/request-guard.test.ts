import { describe, expect, it } from "vitest";
import { createRequestGuard } from "../request-guard";

describe("createRequestGuard", () => {
    it("keeps the newest request current", () => {
        const guard = createRequestGuard();

        const first = guard.begin();
        expect(first()).toBe(true);

        const second = guard.begin();
        expect(first()).toBe(false);
        expect(second()).toBe(true);
    });

    it("does not resurrect an older request when a newer one finishes", () => {
        const guard = createRequestGuard();

        const first = guard.begin();
        const second = guard.begin();

        // The newer request resolving does not hand the slot back.
        expect(second()).toBe(true);
        expect(first()).toBe(false);
    });

    it("treats everything as stale once retired", () => {
        const guard = createRequestGuard();

        const inFlight = guard.begin();
        guard.retire();

        expect(inFlight()).toBe(false);
        expect(guard.begin()()).toBe(false);
    });
});
