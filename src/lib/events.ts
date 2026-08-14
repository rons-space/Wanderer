import { listen, type EventCallback, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Subscribe to a Tauri event for the lifetime of an effect.
 *
 * `listen` resolves asynchronously, so the obvious pattern
 *
 * ```ts
 * let unlisten: UnlistenFn | undefined;
 * listen(name, handler).then((u) => { unlisten = u; });
 * return () => unlisten?.();
 * ```
 *
 * leaks whenever the component unmounts before the registration resolves: the
 * cleanup sees `undefined` and the handler stays attached to a dead component,
 * calling `setState` on it for the rest of the session. Returning a cleanup that
 * awaits the same promise means the unsubscribe happens either way.
 */
export function subscribe<T>(event: string, handler: EventCallback<T>): () => void {
    const pending: Promise<UnlistenFn> = listen<T>(event, handler);

    return () => {
        pending.then((unlisten) => unlisten()).catch(() => {
            // The registration itself failed, so there is nothing to detach.
        });
    };
}

/** Combine several subscriptions into a single effect cleanup. */
export function subscribeAll(unsubscribers: Array<() => void>): () => void {
    return () => {
        for (const unsubscribe of unsubscribers) {
            unsubscribe();
        }
    };
}
