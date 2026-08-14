import { useCallback, useEffect, useRef } from "react";
import { createRequestGuard, type RequestGuard } from "@/lib/request-guard";

/**
 * Guard against out-of-order async results.
 *
 * Every view here fetches on a changing input (a search term, a selected tag, the
 * item open in the viewer) and writes whatever comes back into state. Nothing
 * cancels or sequences those fetches, so a slow first request that resolves after
 * a fast second one overwrites the newer data. In encrypted mode, where a fetch
 * also decrypts and materialises a file, that means the viewer can show a photo
 * the user is no longer looking at.
 *
 * Call `begin()` before starting a request and use the returned predicate before
 * writing state:
 *
 * ```ts
 * const isCurrent = begin();
 * const items = await api.getMediaByTag(tag);
 * if (!isCurrent()) return;
 * setItems(items);
 * ```
 *
 * The predicate is also false after unmount, so it doubles as the "don't set state
 * on a dead component" check.
 */
export function useLatestRequest(): () => () => boolean {
    const guard = useRef<RequestGuard | null>(null);

    if (guard.current === null) {
        guard.current = createRequestGuard();
    }

    useEffect(() => {
        const current = guard.current;
        return () => current?.retire();
    }, []);

    return useCallback(() => {
        // Non-null by construction: the ref is filled on first render, above.
        return guard.current!.begin();
    }, []);
}
