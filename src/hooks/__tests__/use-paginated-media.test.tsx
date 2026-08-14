import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MediaItem } from "@/types";
import { MediaFetcher, usePaginatedMedia } from "@/hooks/use-paginated-media";

const item = (id: number): MediaItem =>
    ({
        id,
        file_path: `/photos/IMG_${id}.jpg`,
        file_name: `IMG_${id}.jpg`,
        mime_type: "image/jpeg",
        created_at: 1_700_000_000,
        rating: 0,
        is_favorite: false,
        is_archived: false,
        is_deleted: false,
        is_cloud_only: false,
    }) as MediaItem;

/** A backend holding `total` rows, answering honestly for any window. */
const pagedFetcher = (total: number): MediaFetcher =>
    vi.fn(async (limit: number, offset: number) =>
        Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, index) =>
            item(offset + index + 1),
        ),
    );

/** A fetcher that resolves only when the test says so. */
const deferredFetcher = () => {
    const pending: Array<(items: MediaItem[]) => void> = [];
    const fetcher: MediaFetcher = vi.fn(
        () => new Promise<MediaItem[]>((resolve) => pending.push(resolve)),
    );
    return { fetcher, pending };
};

beforeEach(() => {
    vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
    vi.restoreAllMocks();
});

describe("usePaginatedMedia", () => {
    it("loads the first page on mount", async () => {
        const fetcher = pagedFetcher(250);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));

        await waitFor(() => expect(result.current.items).toHaveLength(10));
        expect(fetcher).toHaveBeenCalledWith(10, 0);
        expect(result.current.hasNextPage).toBe(true);
    });

    it("knows there is no next page when the first one comes back short", async () => {
        const fetcher = pagedFetcher(4);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));

        await waitFor(() => expect(result.current.items).toHaveLength(4));
        expect(result.current.hasNextPage).toBe(false);
    });

    it("appends the next page", async () => {
        const fetcher = pagedFetcher(25);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));
        await waitFor(() => expect(result.current.items).toHaveLength(10));

        await act(async () => {
            await result.current.loadNextPage(10, 20);
        });

        expect(result.current.items.map((row) => row.id)).toEqual(
            Array.from({ length: 20 }, (_, index) => index + 1),
        );
        expect(result.current.hasNextPage).toBe(true);
    });

    it("drops ids it already holds, which is what an insert between pages produces", async () => {
        // The window shifted by two rows between the two requests, so the second
        // page repeats the last two ids of the first.
        const fetcher: MediaFetcher = vi
            .fn<MediaFetcher>()
            .mockResolvedValueOnce([item(1), item(2), item(3), item(4)])
            .mockResolvedValueOnce([item(3), item(4), item(5), item(6)]);

        const { result } = renderHook(() => usePaginatedMedia(fetcher, 4));
        await waitFor(() => expect(result.current.items).toHaveLength(4));

        await act(async () => {
            await result.current.loadNextPage(4, 8);
        });

        expect(result.current.items.map((row) => row.id)).toEqual([1, 2, 3, 4, 5, 6]);
        expect(new Set(result.current.items.map((row) => row.id)).size).toBe(6);
    });

    it("stops paging when a page comes back empty", async () => {
        const fetcher = pagedFetcher(10);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));
        await waitFor(() => expect(result.current.items).toHaveLength(10));

        await act(async () => {
            await result.current.loadNextPage(10, 20);
        });

        expect(result.current.hasNextPage).toBe(false);
        expect(result.current.items).toHaveLength(10);
    });

    it("refuses to page again once it knows the end has been reached", async () => {
        const fetcher = pagedFetcher(4);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));
        await waitFor(() => expect(result.current.hasNextPage).toBe(false));

        await act(async () => {
            await result.current.loadNextPage(4, 14);
        });

        expect(fetcher).toHaveBeenCalledTimes(1);
    });

    it("keeps the loaded window on refresh instead of collapsing to one page", async () => {
        const fetcher = pagedFetcher(60);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));
        await waitFor(() => expect(result.current.items).toHaveLength(10));

        await act(async () => {
            await result.current.loadNextPage(10, 30);
        });
        expect(result.current.items).toHaveLength(30);

        await act(async () => {
            await result.current.refresh();
        });

        expect(fetcher).toHaveBeenLastCalledWith(30, 0);
        expect(result.current.items).toHaveLength(30);
    });

    it("survives a fetcher that rejects, keeping what it already had", async () => {
        const fetcher = vi
            .fn<MediaFetcher>()
            .mockResolvedValueOnce([item(1), item(2)])
            .mockRejectedValueOnce(new Error("vault locked"));

        const { result } = renderHook(() => usePaginatedMedia(fetcher, 2));
        await waitFor(() => expect(result.current.items).toHaveLength(2));

        await act(async () => {
            await result.current.loadNextPage(2, 4);
        });

        expect(result.current.items.map((row) => row.id)).toEqual([1, 2]);
        // The flag has to come back down, or the view can never page again.
        expect(result.current.isNextPageLoading).toBe(false);
    });

    it("applies an optimistic update without a round trip", async () => {
        const fetcher = pagedFetcher(3);
        const { result } = renderHook(() => usePaginatedMedia(fetcher, 10));
        await waitFor(() => expect(result.current.items).toHaveLength(3));

        act(() => {
            result.current.update((items) => items.filter((row) => row.id !== 2));
        });

        expect(result.current.items.map((row) => row.id)).toEqual([1, 3]);
        expect(fetcher).toHaveBeenCalledTimes(1);
    });

    it("ignores a first page that lands after the view has moved on", async () => {
        const slow = deferredFetcher();
        const fast = deferredFetcher();

        const { result, rerender } = renderHook(({ fetcher }) => usePaginatedMedia(fetcher, 10), {
            initialProps: { fetcher: slow.fetcher },
        });

        // Switching album (or tab) swaps the fetcher while the first is in flight.
        rerender({ fetcher: fast.fetcher });

        await act(async () => {
            fast.pending[0]([item(50)]);
        });
        await act(async () => {
            slow.pending[0]([item(1), item(2), item(3)]);
        });

        expect(result.current.items.map((row) => row.id)).toEqual([50]);
    });

    it("ignores a refresh that lands after a later one", async () => {
        const deferred = deferredFetcher();
        const { result } = renderHook(() => usePaginatedMedia(deferred.fetcher, 10));

        await act(async () => {
            deferred.pending[0]([item(1)]);
        });
        expect(result.current.items.map((row) => row.id)).toEqual([1]);

        let firstRefresh: Promise<void> | undefined;
        let secondRefresh: Promise<void> | undefined;
        act(() => {
            firstRefresh = result.current.refresh();
            secondRefresh = result.current.refresh();
        });

        await act(async () => {
            // The newer refresh answers first, the older one afterwards.
            deferred.pending[2]([item(9)]);
            deferred.pending[1]([item(1), item(2)]);
            await Promise.all([firstRefresh, secondRefresh]);
        });

        expect(result.current.items.map((row) => row.id)).toEqual([9]);
    });
});
