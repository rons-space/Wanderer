import { api } from "./api";

/**
 * Everything an error boundary cannot see.
 *
 * A boundary only catches errors thrown while rendering. A rejected promise in
 * an event handler, or a throw from a `setTimeout` callback, gets nowhere near
 * one, and in a packaged build the console it lands in does not exist. These
 * two handlers put those in the log too.
 *
 * Returns a teardown so tests and hot reloads do not stack listeners.
 */
export function installCrashReporting(target: Window = window): () => void {
    const onError = (event: ErrorEvent) => {
        void api.reportFrontendError(
            "window.onerror",
            event.message || String(event.error),
            event.error instanceof Error ? event.error.stack : undefined,
        );
    };

    const onRejection = (event: PromiseRejectionEvent) => {
        const reason = event.reason;
        void api.reportFrontendError(
            "unhandledrejection",
            reason instanceof Error ? reason.message : String(reason),
            reason instanceof Error ? reason.stack : undefined,
        );
    };

    target.addEventListener("error", onError);
    target.addEventListener("unhandledrejection", onRejection);

    return () => {
        target.removeEventListener("error", onError);
        target.removeEventListener("unhandledrejection", onRejection);
    };
}
