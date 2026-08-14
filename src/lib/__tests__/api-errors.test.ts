import { describe, expect, it } from "vitest";
import { asAppError, errorMessage, hasErrorCode } from "@/lib/api";

describe("asAppError", () => {
    it("recognises the shape the backend serializes", () => {
        expect(asAppError({ code: "vaultLocked", message: "Vault is locked" })).toEqual({
            code: "vaultLocked",
            message: "Vault is locked",
        });
    });

    it("rejects anything else, including near misses", () => {
        expect(asAppError("Vault is locked")).toBeNull();
        expect(asAppError(new Error("Vault is locked"))).toBeNull();
        expect(asAppError(null)).toBeNull();
        expect(asAppError({ code: "vaultLocked" })).toBeNull();
        expect(asAppError({ code: 7, message: "Vault is locked" })).toBeNull();
    });
});

describe("hasErrorCode", () => {
    it("matches on the code and not on the wording", () => {
        const error = { code: "notFound" as const, message: "No such media" };
        expect(hasErrorCode(error, "notFound")).toBe(true);
        expect(hasErrorCode(error, "database")).toBe(false);
        expect(hasErrorCode("No such media", "notFound")).toBe(false);
    });
});

describe("errorMessage", () => {
    it("prefers the backend message", () => {
        expect(errorMessage({ code: "io", message: "Disk is full" })).toBe("Disk is full");
    });

    it("unwraps a thrown Error", () => {
        expect(errorMessage(new Error("Disk is full"))).toBe("Disk is full");
    });

    it("passes a plain string rejection through", () => {
        expect(errorMessage("Disk is full")).toBe("Disk is full");
    });

    it("strips the Error: prefix that would otherwise show inside a toast", () => {
        expect(errorMessage("Error: Disk is full")).toBe("Disk is full");
        expect(errorMessage("Error:Disk is full")).toBe("Disk is full");
    });

    it("leaves a message that merely mentions an error alone", () => {
        expect(errorMessage("Errors were found")).toBe("Errors were found");
        expect(errorMessage("Upload error: timed out")).toBe("Upload error: timed out");
    });

    it("still says something for a rejection with no message at all", () => {
        expect(errorMessage(undefined)).toBe("undefined");
        expect(errorMessage(null)).toBe("null");
    });
});
