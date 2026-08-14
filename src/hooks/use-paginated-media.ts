import { useCallback, useEffect, useRef, useState } from "react";
import { MediaItem } from "@/types";
import { useLatestRequest } from "@/hooks/use-latest-request";

/** Fetches one page. `limit` and `offset` are what the backend commands take. */
export type MediaFetcher = (limit: number, offset: number) => Promise<MediaItem[]>;

export interface PaginatedMedia {
    items: MediaItem[];
    hasNextPage: boolean;
    isNextPageLoading: boolean;
    loadNextPage: (startIndex: number, stopIndex: number) => Promise<void>;
    /** Re-read from the first page up to what is currently loaded. */
    refresh: () => Promise<void>;
    /** Apply a local change without a round trip (optimistic updates). */
    update: (updater: (items: MediaItem[]) => MediaItem[]) => void;
}

/** Rows requested for the first page, and the floor for a refresh. */
export const DEFAULT_PAGE_SIZE = 100;

/**
 * The paginated-list half of every media view.
 *
 * `Favorites`, `Archive`, `Trash`, `Gallery` and the rest each had their own copy
 * of this: four pieces of state, a first-page effect, a `loadNextPage` and a
 * refresh, differing only in which API they called. Three of those copies appended
 * pages without checking for ids they already held, which is where the duplicate
 * React keys came from when a page boundary shifted under an insert.
 *
 * `fetcher` has to be stable, normally a `useCallback`. Changing it re-reads the
 * first page, which is what switching album or tab wants; an inline arrow would
 * make that happen on every render and never stop.
 */
export function usePaginatedMedia(
    fetcher: MediaFetcher,
    pageSize: number = DEFAULT_PAGE_SIZE,
): PaginatedMedia {
    const [items, setItems] = useState<MediaItem[]>([]);
    const [hasNextPage, setHasNextPage] = useState(true);
    const [isNextPageLoading, setIsNextPageLoading] = useState(false);
    const beginRequest = useLatestRequest();

    // Read in callbacks that must not change identity when the list does.
    const loadedCount = useRef(0);
    useEffect(() => {
        loadedCount.current = items.length;
    }, [items]);

    const loadFirstPage = useCallback(async () => {
        const isCurrent = beginRequest();
        try {
            const page = await fetcher(pageSize, 0);
            if (!isCurrent()) return;
            setItems(page);
            setHasNextPage(page.length >= pageSize);
        } catch (e) {
            console.error("Failed to load media", e);
        }
    }, [fetcher, pageSize, beginRequest]);

    useEffect(() => {
        loadFirstPage();
    }, [loadFirstPage]);

    const loadNextPage = useCallback(
        async (startIndex: number, stopIndex: number) => {
            if (isNextPageLoading || !hasNextPage) return;

            const limit = Math.max(stopIndex - startIndex, 1);
            setIsNextPageLoading(true);
            try {
                const page = await fetcher(limit, startIndex);

                if (page.length === 0) {
                    setHasNextPage(false);
                    return;
                }

                setItems((prev) => {
                    // Dedupe on append: an insert between two requests shifts the
                    // offsets, and the same row comes back in two pages. React then
                    // renders two elements with the same key.
                    const seen = new Set(prev.map((item) => item.id));
                    return [...prev, ...page.filter((item) => !seen.has(item.id))];
                });
                setHasNextPage(page.length >= limit);
            } catch (e) {
                console.error("Failed to load more media", e);
            } finally {
                setIsNextPageLoading(false);
            }
        },
        [fetcher, hasNextPage, isNextPageLoading],
    );

    const refresh = useCallback(async () => {
        const isCurrent = beginRequest();
        // Keep the window the user has scrolled through rather than collapsing to
        // the first page, which would throw away their position.
        const limit = Math.max(loadedCount.current, pageSize);
        try {
            const page = await fetcher(limit, 0);
            if (!isCurrent()) return;
            setItems(page);
            setHasNextPage(page.length >= limit);
        } catch (e) {
            console.error("Failed to refresh media", e);
        }
    }, [fetcher, pageSize, beginRequest]);

    const update = useCallback((updater: (items: MediaItem[]) => MediaItem[]) => {
        setItems((prev) => updater(prev));
    }, []);

    return { items, hasNextPage, isNextPageLoading, loadNextPage, refresh, update };
}
