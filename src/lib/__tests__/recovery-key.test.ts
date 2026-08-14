import { describe, expect, it } from "vitest";
import { splitRecoveryKey, verificationIndexes, verifySegments } from "../recovery-key";

describe("splitRecoveryKey", () => {
    it("splits on dashes and drops the empty pieces", () => {
        expect(splitRecoveryKey("  ABCD-EFGH--IJKL  ")).toEqual(["ABCD", "EFGH", "IJKL"]);
    });

    it("returns nothing for an empty key", () => {
        expect(splitRecoveryKey("   ")).toEqual([]);
    });
});

describe("verificationIndexes", () => {
    it("asks for an interior pair when the key is long enough", () => {
        expect(verificationIndexes(8)).toEqual([1, 6]);
    });

    it("asks for both segments of a two-segment key", () => {
        expect(verificationIndexes(2)).toEqual([0, 1]);
    });

    it("degrades to the only segment there is", () => {
        expect(verificationIndexes(1)).toEqual([0, 0]);
        expect(verificationIndexes(0)).toEqual([0, 0]);
    });
});

describe("verifySegments", () => {
    const segments = ["AAAA", "BBBB", "CCCC", "DDDD"];

    it("accepts the right segments whatever the case and spacing", () => {
        expect(verifySegments(segments, [1, 2], [" bbbb ", "cCcC"])).toBe(true);
    });

    it("rejects a wrong segment", () => {
        expect(verifySegments(segments, [1, 2], ["BBBB", "DDDD"])).toBe(false);
    });

    it("rejects an empty answer rather than treating it as a match", () => {
        expect(verifySegments(segments, [1, 2], ["", ""])).toBe(false);
    });

    it("rejects indexes that fall outside the key", () => {
        expect(verifySegments(segments, [1, 9], ["BBBB", ""])).toBe(false);
    });

    it("rejects everything when there is no key", () => {
        expect(verifySegments([], [0, 0], ["", ""])).toBe(false);
    });
});
