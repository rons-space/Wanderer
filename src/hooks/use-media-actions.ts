import { useMemo } from "react";
import { toast } from "sonner";
import { api, errorMessage } from "@/lib/api";
import { MediaItem } from "@/types";

/** The mutations the grid's context menu and overlays can trigger on an item. */
export interface MediaActions {
    addToAlbum: (mediaId: number, albumId: number) => Promise<void>;
    toggleFavorite: (item: MediaItem) => Promise<void>;
    setRating: (mediaId: number, rating: number) => Promise<void>;
    remove: (mediaId: number) => Promise<void>;
    archive: (mediaId: number) => Promise<void>;
    unarchive: (mediaId: number) => Promise<void>;
    removeLocalCopy: (mediaId: number) => Promise<void>;
    downloadLocalCopy: (mediaId: number) => Promise<void>;
}

type Update = (updater: (items: MediaItem[]) => MediaItem[]) => void;

/**
 * The eight item mutations, wired to whoever owns the list.
 *
 * These used to live inside `MediaGrid` and write to a `localItems` copy of the
 * `items` prop. That copy is why a mutation could disagree with its owner: the
 * grid dropped an archived item from its own state, the owner still had it, and
 * the next prop update put it back. The owner passes `update` here and holds the
 * only copy.
 */
export function useMediaActions(update: Update, onItemsChange?: () => void): MediaActions {
    return useMemo<MediaActions>(() => {
        const patch = (mediaId: number, changes: Partial<MediaItem>) =>
            update((items) =>
                items.map((item) => (item.id === mediaId ? { ...item, ...changes } : item)),
            );

        const drop = (mediaId: number) =>
            update((items) => items.filter((item) => item.id !== mediaId));

        return {
            addToAlbum: async (mediaId, albumId) => {
                try {
                    await api.addMediaToAlbum(albumId, mediaId);
                    toast.success("Added to album");
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to add to album");
                }
            },

            toggleFavorite: async (item) => {
                try {
                    const isFavorite = await api.toggleFavorite(item.id);
                    patch(item.id, { is_favorite: isFavorite });
                    toast.success(isFavorite ? "Added to favorites" : "Removed from favorites");
                    // The favorites view has to drop the item, so tell the owner.
                    if (!isFavorite) {
                        onItemsChange?.();
                    }
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to update favorite");
                }
            },

            setRating: async (mediaId, rating) => {
                try {
                    await api.setRating(mediaId, rating);
                    patch(mediaId, { rating });
                    toast.success(rating > 0 ? `Rated ${rating} stars` : "Rating removed");
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to set rating");
                }
            },

            remove: async (mediaId) => {
                try {
                    await api.softDeleteMedia(mediaId);
                    drop(mediaId);
                    toast.success("Moved to trash");
                    onItemsChange?.();
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to move to trash");
                }
            },

            archive: async (mediaId) => {
                try {
                    await api.archiveMedia(mediaId);
                    drop(mediaId);
                    toast.success("Archived");
                    onItemsChange?.();
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to archive");
                }
            },

            unarchive: async (mediaId) => {
                try {
                    await api.unarchiveMedia(mediaId);
                    // Leaving the archive view, so drop it rather than flipping a
                    // flag on a row that view will not show again.
                    drop(mediaId);
                    toast.success("Unarchived");
                    onItemsChange?.();
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to unarchive");
                }
            },

            removeLocalCopy: async (mediaId) => {
                try {
                    await api.removeLocalCopy(mediaId);
                    patch(mediaId, { is_cloud_only: true });
                    toast.success("Local copy removed (Cloud Only)");
                } catch (e) {
                    console.error(e);
                    toast.error("Failed to remove local copy");
                }
            },

            downloadLocalCopy: async (mediaId) => {
                toast.promise(api.downloadLocalCopy(mediaId), {
                    loading: "Downloading...",
                    success: () => {
                        patch(mediaId, { is_cloud_only: false });
                        return "Downloaded local copy";
                    },
                    error: (err) => `Failed to download: ${errorMessage(err)}`,
                });
            },
        };
    }, [update, onItemsChange]);
}
