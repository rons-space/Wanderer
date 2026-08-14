import { ComponentType, ReactNode, useCallback, useMemo, useState } from "react";
import { LucideIcon } from "lucide-react";
import { MediaItem } from "@/types";
import { MediaGrid } from "./MediaGrid";
import { MediaViewer } from "./MediaViewer";
import { usePaginatedMedia, type MediaFetcher } from "@/hooks/use-paginated-media";
import { useMediaActions } from "@/hooks/use-media-actions";

/** What a view's render props get, so they can drive the list they are inside. */
export interface MediaListControls {
    items: MediaItem[];
    /** Re-read the loaded window from the backend. */
    refresh: () => Promise<void>;
    /** Drop an item locally, after the backend call that removed it succeeded. */
    drop: (mediaId: number) => void;
    /** Empty the list locally, for the operations that remove everything. */
    clear: () => void;
}

interface MediaListViewProps {
    /** One page of items. Must be stable, or the first page refetches every render. */
    fetcher: MediaFetcher;
    title: string;
    icon: LucideIcon;
    /** Tailwind classes for the icon and its circle, per view. */
    iconClassName?: string;
    iconWrapperClassName?: string;
    /** Rendered instead of the item count when a view has more to say. */
    subtitle?: (count: number) => ReactNode;
    /** Rendered to the right of the heading, e.g. an Empty Trash button. */
    headerActions?: (controls: MediaListControls) => ReactNode;
    emptyTitle: string;
    emptyDescription: string;
    /** Extra entries for the grid's context menu, e.g. Restore in Trash. */
    contextMenuExtras?: (item: MediaItem, controls: MediaListControls) => ReactNode;
    ItemWrapper?: ComponentType<{ item: MediaItem; children: ReactNode }>;
}

/**
 * The shell every simple media view was a copy of: header, empty state, grid and
 * viewer, over a paginated fetch.
 *
 * `Favorites`, `Archive` and `Trash` were the same 98 lines with a different icon
 * and a different `api` call, which is also how three of them ended up sharing the
 * same duplicate-key pagination bug.
 */
export function MediaListView({
    fetcher,
    title,
    icon: Icon,
    iconClassName = "w-5 h-5 text-muted-foreground",
    iconWrapperClassName = "bg-muted",
    subtitle,
    headerActions,
    emptyTitle,
    emptyDescription,
    contextMenuExtras,
    ItemWrapper,
}: MediaListViewProps) {
    const [selectedItem, setSelectedItem] = useState<MediaItem | null>(null);
    const { items, hasNextPage, isNextPageLoading, loadNextPage, refresh, update } =
        usePaginatedMedia(fetcher);
    const actions = useMediaActions(update, refresh);

    const handleItemClick = useCallback((item: MediaItem) => {
        setSelectedItem(item);
    }, []);

    const controls = useMemo<MediaListControls>(
        () => ({
            items,
            refresh,
            drop: (mediaId: number) =>
                update((current) => current.filter((item) => item.id !== mediaId)),
            clear: () => update(() => []),
        }),
        [items, refresh, update],
    );

    const menuExtras = useMemo(
        () =>
            contextMenuExtras
                ? (item: MediaItem) => contextMenuExtras(item, controls)
                : undefined,
        [contextMenuExtras, controls],
    );

    return (
        <div className="flex flex-col h-full">
            <div className="flex items-center justify-between p-4 border-b">
                <div className="flex items-center gap-3">
                    <div
                        className={`flex items-center justify-center w-10 h-10 rounded-full ${iconWrapperClassName}`}
                    >
                        <Icon className={iconClassName} aria-hidden="true" />
                    </div>
                    <div>
                        <h1 className="text-lg font-semibold">{title}</h1>
                        <p className="text-sm text-muted-foreground">
                            {subtitle
                                ? subtitle(items.length)
                                : `${items.length} ${items.length === 1 ? "item" : "items"}`}
                        </p>
                    </div>
                </div>

                {headerActions?.(controls)}
            </div>

            {items.length === 0 ? (
                <div className="flex-1 flex items-center justify-center">
                    <div className="text-center">
                        <Icon
                            className="w-16 h-16 mx-auto mb-4 text-muted-foreground/30"
                            aria-hidden="true"
                        />
                        <h2 className="text-lg font-medium text-muted-foreground">{emptyTitle}</h2>
                        <p className="text-sm text-muted-foreground/60">{emptyDescription}</p>
                    </div>
                </div>
            ) : (
                <MediaGrid
                    items={items}
                    hasNextPage={hasNextPage}
                    isNextPageLoading={isNextPageLoading}
                    loadNextPage={loadNextPage}
                    onItemClick={handleItemClick}
                    contextMenuExtras={menuExtras}
                    ItemWrapper={ItemWrapper}
                    actions={actions}
                />
            )}

            {selectedItem && (
                <MediaViewer
                    item={selectedItem}
                    open={!!selectedItem}
                    onClose={() => setSelectedItem(null)}
                    items={items}
                    onNavigate={setSelectedItem}
                />
            )}
        </div>
    );
}
