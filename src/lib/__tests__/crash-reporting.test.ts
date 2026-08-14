import { beforeEach, describe, expect, it, vi } from "vitest";
import { installCrashReporting } from "../crash-reporting";
import { api } from "../api";

// The real module reaches for the Tauri IPC bridge, which does not exist in a
// plain node test run.
vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn(async () => undefined),
    convertFileSrc: (path: string) => path,
}));

const report = vi.spyOn(api, "reportFrontendError").mockResolvedValue(undefined);

/** Stands in for `window`, which the node test environment does not provide. */
function fakeWindow() {
    const target = new EventTarget();
    return target as unknown as Window;
}

describe("installCrashReporting", () => {
    beforeEach(() => {
        report.mockClear();
    });

    it("reports an uncaught error with its stack", () => {
        const target = fakeWindow();
        installCrashReporting(target);

        const error = new Error("boom");
        target.dispatchEvent(Object.assign(new Event("error"), { message: "boom", error }));

        expect(report).toHaveBeenCalledWith("window.onerror", "boom", error.stack);
    });

    it("reports a rejected promise", () => {
        const target = fakeWindow();
        installCrashReporting(target);

        const reason = new Error("no network");
        target.dispatchEvent(Object.assign(new Event("unhandledrejection"), { reason }));

        expect(report).toHaveBeenCalledWith("unhandledrejection", "no network", reason.stack);
    });

    it("describes a rejection that is not an Error", () => {
        const target = fakeWindow();
        installCrashReporting(target);

        target.dispatchEvent(Object.assign(new Event("unhandledrejection"), { reason: "just a string" }));

        expect(report).toHaveBeenCalledWith("unhandledrejection", "just a string", undefined);
    });

    it("stops reporting once torn down, so reloads do not stack listeners", () => {
        const target = fakeWindow();
        const teardown = installCrashReporting(target);
        teardown();

        target.dispatchEvent(Object.assign(new Event("error"), { message: "boom" }));

        expect(report).not.toHaveBeenCalled();
    });
});
