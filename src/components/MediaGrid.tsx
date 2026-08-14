import { useState, useRef, useLayoutEffect, useCallback, ComponentType, ReactNode, useMemo, useEffect } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { MediaItem, Album } from "../types";
import { Skeleton } from "@/components/ui/skeleton";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import {
    ContextMenu,
    ContextMenuContent,
    ContextMenuItem,
    ContextMenuTrigger,
    ContextMenuSub,
    ContextMenuSubContent,
    ContextMenuSubTrigger,
    ContextMenuSeparator,
} from "@/components/ui/context-menu";
import { api, errorMessage } from "@/lib/api";
import { handleImageError } from "@/lib/placeholder";
import { toast } from "sonner";
import { useTheme, type ThemeVariant } from "@/contexts/ThemeContext";
import { Play, Heart, Star, Trash2, Archive, ArchiveRestore, Download, Cloud, Share2, Calendar } from "lucide-react";

// --- Date Separator Helpers ---
type TimelineGrouping = 'day' | 'month' | 'year';
type SeparatorRow = {
    type: 'separator';
    dateKey: string;
    label: string;
    firstItemIndex: number;
};
type ItemsRow = {
    type: 'items';
    dateKey: string;
    startIndex: number;
    count: number;
};
type DisplayRow = SeparatorRow | ItemsRow;

const getDateKey = (timestamp: number, grouping: TimelineGrouping): string => {
    const date = new Date(timestamp * 1000);
    switch (grouping) {
        case 'year':
            return date.getFullYear().toString();
        case 'month':
            return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}`;
        case 'day':
        default:
            return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
    }
};

const formatDateKey = (dateKey: string, grouping: TimelineGrouping): string => {
    const parts = dateKey.split('-');
    const monthNames = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

    switch (grouping) {
        case 'year':
            return parts[0];
        case 'month':
            return `${monthNames[parseInt(parts[1]) - 1]} ${parts[0]}`;
        case 'day':
        default:
            return `${monthNames[parseInt(parts[1]) - 1]} ${parseInt(parts[2])}, ${parts[0]}`;
    }
};

const parseDateTakenToTimestamp = (dateTaken?: string): number | null => {
    if (!dateTaken) {
        return null;
    }

    // Support "YYYY-MM-DD HH:mm:ss" and "YYYY:MM:DD HH:mm:ss" variants.
    const normalized = dateTaken
        .trim()
        .replace(/^(\d{4}):(\d{2}):(\d{2})/, "$1-$2-$3")
        .replace(" ", "T");
    const parsed = Date.parse(normalized);
    if (Number.isNaN(parsed)) {
        return null;
    }

    return Math.floor(parsed / 1000);
};

const getTimelineTimestamp = (item: MediaItem): number => {
    return parseDateTakenToTimestamp(item.date_taken) ?? item.created_at;
};

// --- Custom AutoSizer ---
const useResizeObserver = (ref: React.RefObject<HTMLElement | null>) => {
    const [dimensions, setDimensions] = useState({ width: 0, height: 0 });

    useLayoutEffect(() => {
        const element = ref.current;
        if (!element) return;

        const observer = new ResizeObserver((entries) => {
            if (!entries || entries.length === 0) return;
            const entry = entries[0];
            const { width, height } = entry.contentRect;
            setDimensions({ width, height });
        });

        observer.observe(element);
        return () => observer.disconnect();
    }, [ref]);

    return dimensions;
};

// --- Virtual Grid Component ---
interface VirtualGridProps {
    items: MediaItem[];
    rows: DisplayRow[];
    columnCount: number;
    columnWidth: number;
    gap: number;
    height: number;
    width: number;
    isNextPageLoading: boolean;
    onScroll: (scrollTop: number, clientHeight: number, scrollHeight: number, visibleItemIndex?: number) => void;
    ItemWrapper?: ComponentType<{ item: MediaItem; children: ReactNode }>;
    contextMenuExtras?: (item: MediaItem) => ReactNode;
    albums: Album[];
    onAddToAlbum: (mediaId: number, albumId: number) => void;
    onItemClick?: (item: MediaItem, e?: React.MouseEvent) => void;
    onToggleFavorite: (item: MediaItem) => void;
    onSetRating: (mediaId: number, rating: number) => void;
    onDelete: (mediaId: number) => void;
    onArchive: (mediaId: number) => void;
    onUnarchive: (mediaId: number) => void;
    onRemoveLocalCopy: (mediaId: number) => void;
    onDownloadLocalCopy: (mediaId: number) => void;
    theme: ThemeVariant;
}

const VirtualGrid = ({
    items,
    rows,
    columnCount,
    columnWidth,
    gap,
    height,
    width,
    isNextPageLoading,
    onScroll,
    ItemWrapper,
    contextMenuExtras,
    albums,
    onAddToAlbum,
    onItemClick,
    onToggleFavorite,
    onSetRating,
    onDelete,
    onArchive,
    onUnarchive,
    onRemoveLocalCopy,
    onDownloadLocalCopy,
    // theme, // Unused
}: VirtualGridProps) => {
    const scrollContainerRef = useRef<HTMLDivElement>(null);
    const [scrollTop, setScrollTop] = useState(0);

    const SEPARATOR_ROW_HEIGHT = 36;
    const ITEM_ROW_HEIGHT = columnWidth + gap;

    // Buffer for smooth scrolling
    const OVERSCAN = 3;

    const rowsWithLoading = useMemo<DisplayRow[]>(() => {
        if (!isNextPageLoading) {
            return rows;
        }

        if (rows.length === 0) {
            return [{ type: 'items', dateKey: '', startIndex: items.length, count: 1 }];
        }

        const nextRows = [...rows];
        const lastRow = nextRows[nextRows.length - 1];

        if (lastRow.type === 'items' && lastRow.count < columnCount) {
            nextRows[nextRows.length - 1] = {
                ...lastRow,
                count: lastRow.count + 1,
            };
            return nextRows;
        }

        nextRows.push({
            type: 'items',
            dateKey: lastRow.dateKey,
            startIndex: items.length,
            count: 1,
        });
        return nextRows;
    }, [rows, isNextPageLoading, items.length, columnCount]);

    const rowHeights = useMemo(
        () => rowsWithLoading.map((row) => (row.type === 'separator' ? SEPARATOR_ROW_HEIGHT : ITEM_ROW_HEIGHT)),
        [rowsWithLoading, ITEM_ROW_HEIGHT]
    );

    const rowOffsets = useMemo(() => {
        const offsets: number[] = [];
        let offset = 0;

        for (const rowHeight of rowHeights) {
            offsets.push(offset);
            offset += rowHeight;
        }

        return offsets;
    }, [rowHeights]);

    const totalHeight = rowHeights.length > 0
        ? rowOffsets[rowOffsets.length - 1] + rowHeights[rowHeights.length - 1]
        : 0;

    const findRowIndexAtOffset = (offset: number): number => {
        if (rowHeights.length === 0) {
            return -1;
        }

        let low = 0;
        let high = rowHeights.length - 1;

        while (low <= high) {
            const mid = Math.floor((low + high) / 2);
            const rowStart = rowOffsets[mid];
            const rowEnd = rowStart + rowHeights[mid];

            if (offset < rowStart) {
                high = mid - 1;
            } else if (offset >= rowEnd) {
                low = mid + 1;
            } else {
                return mid;
            }
        }

        return Math.max(0, Math.min(rowHeights.length - 1, low));
    };

    // Calculate visible range
    const rawVisibleStart = findRowIndexAtOffset(scrollTop);
    const rawVisibleEnd = findRowIndexAtOffset(scrollTop + height);
    const visibleRowStart = rawVisibleStart === -1 ? 0 : Math.max(0, rawVisibleStart - OVERSCAN);
    const visibleRowEnd = rawVisibleEnd === -1 ? -1 : Math.min(rowsWithLoading.length - 1, rawVisibleEnd + OVERSCAN);

    const visibleRows = [];
    if (visibleRowEnd >= visibleRowStart) {
        for (let i = visibleRowStart; i <= visibleRowEnd; i++) {
            visibleRows.push(i);
        }
    }


    // A viewport taller than the loaded content never fires a scroll event, so
    // pagination driven purely by scrolling deadlocks on a large window: the
    // first page does not fill the screen, and nothing asks for the second.
    // Report the metrics directly whenever the content stops overflowing.
    useEffect(() => {
        if (height <= 0 || totalHeight <= 0 || totalHeight > height) {
            return;
        }

        onScroll(0, height, totalHeight, 0);
    }, [totalHeight, height, onScroll]);

    const handleScroll = (e: React.UIEvent<HTMLDivElement>) => {
        const { scrollTop, clientHeight, scrollHeight } = e.currentTarget;
        setScrollTop(scrollTop);

        const visibleRowIndex = findRowIndexAtOffset(scrollTop);
        let visibleItemIndex: number | undefined;

        if (visibleRowIndex >= 0) {
            for (let index = visibleRowIndex; index < rowsWithLoading.length; index += 1) {
                const row = rowsWithLoading[index];

                if (row.type === 'separator') {
                    visibleItemIndex = row.firstItemIndex;
                    break;
                }

                if (items.length > 0) {
                    visibleItemIndex = Math.min(row.startIndex, items.length - 1);
                }
                break;
            }
        }

        onScroll(scrollTop, clientHeight, scrollHeight, visibleItemIndex);
    };



    return (
        <div
            ref={scrollContainerRef}
            className="w-full h-full overflow-y-auto overflow-x-hidden relative p-4"
            onScroll={handleScroll}
            style={{ width, height }}
        >
            <div style={{ height: totalHeight, width: "100%", position: "relative" }}>
                {visibleRows.map((rowIndex) => {
                    const row = rowsWithLoading[rowIndex];
                    const rowTop = rowOffsets[rowIndex];
                    const rowHeight = rowHeights[rowIndex];

                    if (row.type === 'separator') {
                        return (
                            <div
                                key={`separator-${row.dateKey}-${row.firstItemIndex}`}
                                style={{
                                    position: "absolute",
                                    left: 0,
                                    top: rowTop,
                                    width: "100%",
                                    height: rowHeight,
                                }}
                                className="flex items-center gap-3 px-0.5"
                            >
                                <Badge
                                    variant="secondary"
                                    className="rounded-md px-2 py-0.5 text-[11px] font-medium uppercase tracking-wide"
                                >
                                    {row.label}
                                </Badge>
                                <Separator className="flex-1" />
                            </div>
                        );
                    }

                    const columns = [];
                    for (let colIndex = 0; colIndex < row.count; colIndex++) {
                        const itemIndex = row.startIndex + colIndex;
                        const item = items[itemIndex];
                        const isLoadingItem = itemIndex >= items.length;

                        // Calculate Position
                        const left = colIndex * (columnWidth + gap);
                        const top = rowTop;

                        columns.push(
                            <div
                                key={itemIndex}
                                style={{
                                    position: "absolute",
                                    left,
                                    top,
                                    width: columnWidth,
                                    height: columnWidth,
                                }}
                            >
                                <Cell
                                    item={item}
                                    isLoading={isLoadingItem}
                                    ItemWrapper={ItemWrapper}
                                    contextMenuExtras={contextMenuExtras}
                                    albums={albums}
                                    onAddToAlbum={onAddToAlbum}
                                    onItemClick={onItemClick}
                                    onToggleFavorite={onToggleFavorite}
                                    onSetRating={onSetRating}
                                    onDelete={onDelete}
                                    onArchive={onArchive}
                                    onUnarchive={onUnarchive}
                                    onRemoveLocalCopy={onRemoveLocalCopy}
                                    onDownloadLocalCopy={onDownloadLocalCopy}
                                />
                            </div>
                        );
                    }
                    return columns;
                })}
            </div>
        </div >
    );
};

// --- Cell Component ---
const Cell = ({
    item,
    isLoading,
    ItemWrapper,
    contextMenuExtras,
    albums,
    onAddToAlbum,
    onItemClick,
    onToggleFavorite,
    onSetRating,
    onDelete,
    onArchive,
    onUnarchive,
    onRemoveLocalCopy,
    onDownloadLocalCopy,
}: {

    item?: MediaItem;
    isLoading: boolean;
    ItemWrapper?: ComponentType<{ item: MediaItem; children: ReactNode }>;
    contextMenuExtras?: (item: MediaItem) => ReactNode;
    albums: Album[];
    onAddToAlbum: (mediaId: number, albumId: number) => void;
    onItemClick?: (item: MediaItem, e?: React.MouseEvent) => void;
    onToggleFavorite: (item: MediaItem) => void;
    onSetRating: (mediaId: number, rating: number) => void;
    onDelete: (mediaId: number) => void;
    onArchive: (mediaId: number) => void;
    onUnarchive: (mediaId: number) => void;
    onRemoveLocalCopy: (mediaId: number) => void;
    onDownloadLocalCopy: (mediaId: number) => void;
}) => {
    if (isLoading || !item) {
        return (
            <div className="w-full h-full">
                <Skeleton className="h-full w-full rounded-xl" />
            </div>
        );
    }

    const imagePath = item.thumbnail_path || item.file_path;
    const src = convertFileSrc(imagePath);

    const content = (
        <div
            className="group relative w-full h-full overflow-hidden rounded-xl border bg-muted shadow-sm transition-all hover:shadow-md cursor-pointer"
            onClick={(e) => onItemClick && item && onItemClick(item, e)}
        >
            <img
                src={src}
                alt={`Media ${item.id}`}
                loading="lazy"
                decoding="async"
                className="w-full h-full object-cover transition-transform duration-300 group-hover:scale-105"
                onError={handleImageError}
            />

            {/* Overlay Gradient */}
            <div className="absolute inset-0 bg-gradient-to-t from-black/40 via-transparent to-transparent opacity-0 transition-opacity group-hover:opacity-100" />

            {/* Favorite Heart Icon - Always visible if favorited, otherwise on hover */}
            <button
                className={`absolute top-2 left-2 flex items-center justify-center rounded-full p-1.5 backdrop-blur-sm transition-all ${item.is_favorite
                    ? 'bg-red-500/80 opacity-100'
                    : 'bg-black/50 opacity-0 group-hover:opacity-100'
                    }`}
                onClick={(e) => {
                    e.stopPropagation();
                    onToggleFavorite(item);
                }}
            >
                <Heart
                    className={`h-3 w-3 transition-colors ${item.is_favorite ? 'fill-white text-white' : 'text-white hover:fill-white'
                        }`}
                />
            </button>

            {/* Star Rating Badge - Only show if rated */}
            {item.rating > 0 && (
                <div className="absolute bottom-2 left-2 flex items-center gap-0.5 rounded-full bg-black/50 px-1.5 py-0.5 backdrop-blur-sm">
                    <Star className="h-3 w-3 fill-yellow-400 text-yellow-400" />
                    <span className="text-xs font-medium text-white">{item.rating}</span>
                </div>
            )}

            {/* Video Indicator */}
            {item.mime_type?.startsWith("video") && (
                <div className="absolute top-2 right-2 flex items-center justify-center rounded-full bg-black/50 p-1.5 backdrop-blur-sm">
                    <Play className="h-3 w-3 fill-white text-white" />
                </div>
            )}

            {/* Cloud Only Indicator */}
            {item.is_cloud_only && (
                <div className="absolute bottom-2 right-2 flex items-center justify-center rounded-full bg-sky-500/80 p-1.5 backdrop-blur-sm shadow-sm">
                    <Cloud className="h-3 w-3 text-white" />
                </div>
            )}
        </div>
    );

    const wrappedContent = ItemWrapper ? <ItemWrapper item={item}>{content}</ItemWrapper> : content;

    return (
        <ContextMenu>
            <ContextMenuTrigger asChild>
                {wrappedContent}
            </ContextMenuTrigger>
            <ContextMenuContent>
                {/* View-specific entries (Trash contributes Restore here). They live
                    in this menu rather than in a wrapper of their own: a second
                    ContextMenu around the same cell nests the two triggers and the
                    inner menu swallows the outer one. */}
                {contextMenuExtras?.(item)}

                {/* Favorite Toggle */}
                <ContextMenuItem onClick={() => onToggleFavorite(item)}>
                    <Heart className={`mr-2 h-4 w-4 ${item.is_favorite ? 'fill-red-500 text-red-500' : ''}`} />
                    {item.is_favorite ? 'Remove from Favorites' : 'Add to Favorites'}
                </ContextMenuItem>

                {/* Star Rating Submenu */}
                <ContextMenuSub>
                    <ContextMenuSubTrigger>
                        <Star className="mr-2 h-4 w-4" />
                        Rate ({item.rating}/5)
                    </ContextMenuSubTrigger>
                    <ContextMenuSubContent className="w-32">
                        {[0, 1, 2, 3, 4, 5].map((rating) => (
                            <ContextMenuItem
                                key={rating}
                                onClick={() => onSetRating(item.id, rating)}
                            >
                                <div className="flex items-center gap-1">
                                    {rating === 0 ? (
                                        <span className="text-muted-foreground">No rating</span>
                                    ) : (
                                        Array.from({ length: rating }).map((_, i) => (
                                            <Star key={i} className="h-3 w-3 fill-yellow-400 text-yellow-400" />
                                        ))
                                    )}
                                </div>
                            </ContextMenuItem>
                        ))}
                    </ContextMenuSubContent>
                </ContextMenuSub>

                <ContextMenuSeparator />

                {/* Add to Album */}
                <ContextMenuSub>
                    <ContextMenuSubTrigger>Add to Album</ContextMenuSubTrigger>
                    <ContextMenuSubContent className="w-48">
                        {albums.length === 0 ? (
                            <div className="px-2 py-1.5 text-sm text-muted-foreground">No albums</div>
                        ) : (
                            albums.map((album) => (
                                <ContextMenuItem
                                    key={album.id}
                                    onClick={() => onAddToAlbum(item.id, album.id)}
                                >
                                    {album.name}
                                </ContextMenuItem>
                            ))
                        )}
                    </ContextMenuSubContent>
                </ContextMenuSub>

                {/* Export to Folder */}
                <ContextMenuItem
                    onClick={async () => {
                        const folder = await open({ directory: true, multiple: false });
                        if (folder) {
                            try {
                                const exported = await api.exportMedia([item.id], folder as string);
                                if (exported > 0) {
                                    toast.success("Exported successfully");
                                } else {
                                    toast.error("Export failed: file unavailable locally and in cloud");
                                }
                            } catch (e) {
                                toast.error(`Export failed: ${errorMessage(e)}`);
                            }
                        }
                    }}
                >
                    <Download className="mr-2 h-4 w-4" />
                    Export to Folder
                </ContextMenuItem>

                {/* Copy Share Link */}
                <ContextMenuItem
                    onClick={async () => {
                        try {
                            const link = await api.generateShareLink(item.id);
                            await navigator.clipboard.writeText(link);
                            toast.success("Share link copied to clipboard");
                        } catch (e) {
                            toast.error(`Failed to generate share link: ${errorMessage(e)}`);
                        }
                    }}
                    disabled={!item.telegram_media_id}
                >
                    <Share2 className="mr-2 h-4 w-4" />
                    Copy Share Link
                </ContextMenuItem>

                <ContextMenuSeparator />

                <ContextMenuSeparator />

                {/* Archive / Unarchive */}
                {item.is_archived ? (
                    <ContextMenuItem
                        onClick={() => onUnarchive(item.id)}
                    >
                        <ArchiveRestore className="mr-2 h-4 w-4" />
                        Unarchive
                    </ContextMenuItem>
                ) : (
                    <ContextMenuItem
                        onClick={() => onArchive(item.id)}
                    >
                        <Archive className="mr-2 h-4 w-4" />
                        Archive
                    </ContextMenuItem>
                )}

                <ContextMenuSeparator />

                <ContextMenuSeparator />

                {/* Cloud Only Actions */}
                {item.telegram_media_id && !item.is_cloud_only && (
                    <ContextMenuItem onClick={() => onRemoveLocalCopy(item.id)}>
                        <Cloud className="mr-2 h-4 w-4" />
                        Remove Local Copy
                    </ContextMenuItem>
                )}
                {item.is_cloud_only && (
                    <ContextMenuItem onClick={() => onDownloadLocalCopy(item.id)}>
                        <Download className="mr-2 h-4 w-4" />
                        Download Local Copy
                    </ContextMenuItem>
                )}

                <ContextMenuSeparator />

                {/* Delete (move to trash) */}
                <ContextMenuItem
                    onClick={() => onDelete(item.id)}
                    className="text-destructive focus:text-destructive"
                >
                    <Trash2 className="mr-2 h-4 w-4" />
                    Move to Trash
                </ContextMenuItem>
            </ContextMenuContent>
        </ContextMenu>
    );
};

// --- Main MediaGrid Export ---

interface MediaGridProps {
    items: MediaItem[];
    hasNextPage: boolean;
    isNextPageLoading: boolean;
    loadNextPage: (startIndex: number, stopIndex: number) => Promise<void>;
    ItemWrapper?: ComponentType<{ item: MediaItem; children: ReactNode }>;
    contextMenuExtras?: (item: MediaItem) => ReactNode;
    onItemClick?: (item: MediaItem, e?: React.MouseEvent) => void;
    onItemsChange?: () => void; // Callback to refresh items after changes
}

export function MediaGrid({ items, hasNextPage, isNextPageLoading, loadNextPage, ItemWrapper, contextMenuExtras, onItemClick, onItemsChange }: MediaGridProps) {
    const containerRef = useRef<HTMLDivElement>(null);
    const { width, height } = useResizeObserver(containerRef);
    const { theme } = useTheme();

    const [albums, setAlbums] = useState<Album[]>([]);
    const [localItems, setLocalItems] = useState<MediaItem[]>(items);
    const [timelineGrouping, setTimelineGrouping] = useState<TimelineGrouping>('day');
    const [currentDateHeader, setCurrentDateHeader] = useState<string | null>(null);
    const [showDateHeader, setShowDateHeader] = useState(false);
    const hideHeaderTimeout = useRef<NodeJS.Timeout | null>(null);

    // Sync local items with props
    useEffect(() => {
        setLocalItems(items);
    }, [items]);

    // The floating date header schedules a hide on every scroll event. Without
    // this, the last one outlives the component and fires setState on it.
    useEffect(
        () => () => {
            if (hideHeaderTimeout.current) {
                clearTimeout(hideHeaderTimeout.current);
            }
        },
        [],
    );

    // Load timeline grouping config
    useEffect(() => {
        let cancelled = false;

        api.getAllConfig().then((data) => {
            if (cancelled) return;
            const grouping = data?.timeline_grouping as TimelineGrouping || 'day';
            setTimelineGrouping(grouping);
        }).catch(console.error);

        return () => {
            cancelled = true;
        };
    }, []);

    useEffect(() => {
        let cancelled = false;

        api.getAlbums().then((loaded) => {
            if (!cancelled) setAlbums(loaded);
        }).catch(console.error);

        return () => {
            cancelled = true;
        };
    }, []);

    const handleAddToAlbum = async (mediaId: number, albumId: number) => {
        try {
            await api.addMediaToAlbum(albumId, mediaId);
            toast.success("Added to album");
        } catch (e) {
            console.error(e);
            toast.error("Failed to add to album");
        }
    };

    const handleToggleFavorite = async (item: MediaItem) => {
        try {
            const newFavoriteState = await api.toggleFavorite(item.id);
            // Optimistic update
            setLocalItems(prev =>
                prev.map(i =>
                    i.id === item.id ? { ...i, is_favorite: newFavoriteState } : i
                )
            );
            toast.success(newFavoriteState ? "Added to favorites" : "Removed from favorites");
            // Notify parent to refresh (especially important for Favorites view)
            if (!newFavoriteState) {
                onItemsChange?.();
            }
        } catch (e) {
            console.error(e);
            toast.error("Failed to update favorite");
        }
    };

    const handleSetRating = async (mediaId: number, rating: number) => {
        try {
            await api.setRating(mediaId, rating);
            // Optimistic update
            setLocalItems(prev =>
                prev.map(i =>
                    i.id === mediaId ? { ...i, rating } : i
                )
            );
            toast.success(rating > 0 ? `Rated ${rating} stars` : "Rating removed");
        } catch (e) {
            console.error(e);
            toast.error("Failed to set rating");
        }
    };

    const handleDelete = async (mediaId: number) => {
        try {
            await api.softDeleteMedia(mediaId);
            // Remove from local state
            setLocalItems(prev => prev.filter(i => i.id !== mediaId));
            toast.success("Moved to trash");
            onItemsChange?.();
        } catch (e) {
            console.error(e);
            toast.error("Failed to move to trash");
        }
    };

    const handleArchive = async (mediaId: number) => {
        try {
            await api.archiveMedia(mediaId);
            // Remove from view immediately (instead of just setting flag)
            setLocalItems(prev => prev.filter(i => i.id !== mediaId));
            toast.success("Archived");
            onItemsChange?.();
        } catch (e) {
            console.error(e);
            toast.error("Failed to archive");
        }
    };

    const handleUnarchive = async (mediaId: number) => {
        try {
            await api.unarchiveMedia(mediaId);
            setLocalItems(prev => prev.map(i => i.id === mediaId ? { ...i, is_archived: false } : i));
            toast.success("Unarchived");
            onItemsChange?.();
        } catch (e) {
            console.error(e);
            toast.error("Failed to unarchive");
        }
    };

    const handleRemoveLocalCopy = async (mediaId: number) => {
        try {
            await api.removeLocalCopy(mediaId);
            setLocalItems(prev => prev.map(i => i.id === mediaId ? { ...i, is_cloud_only: true } : i));
            toast.success("Local copy removed (Cloud Only)");
        } catch (e) {
            console.error(e);
            toast.error("Failed to remove local copy");
        }
    };

    const handleDownloadLocalCopy = async (mediaId: number) => {
        try {
            toast.promise(api.downloadLocalCopy(mediaId), {
                loading: 'Downloading...',
                success: () => {
                    setLocalItems(prev => prev.map(i => i.id === mediaId ? { ...i, is_cloud_only: false } : i));
                    return "Downloaded local copy";
                },
                error: (err) => `Failed to download: ${errorMessage(err)}`
            });
        } catch (e) {
            console.error(e);
        }
    };

    const GAP = 16;
    const MIN_COLUMN_WIDTH = 180;
    const PADDING_LEFT = 16;  // p-4 = 16px
    const PADDING_RIGHT = 16; // p-4 = 16px
    const SCROLLBAR_WIDTH = 8;

    // Available width for grid content (subtract padding and scrollbar)
    const contentWidth = width - PADDING_LEFT - PADDING_RIGHT - SCROLLBAR_WIDTH;

    const columnCount = Math.max(1, Math.floor(contentWidth / (MIN_COLUMN_WIDTH + GAP)));
    // Adjust column width to fill available content space evenly
    // contentWidth = columnCount * columnWidth + (columnCount - 1) * gap
    // columnWidth = (contentWidth - (columnCount - 1) * gap) / columnCount
    const columnWidth = Math.floor((contentWidth - ((columnCount - 1) * GAP)) / columnCount);

    const displayRows = useMemo<DisplayRow[]>(() => {
        if (localItems.length === 0 || columnCount <= 0) {
            return [];
        }

        const rows: DisplayRow[] = [];
        let index = 0;

        while (index < localItems.length) {
            const dateKey = getDateKey(getTimelineTimestamp(localItems[index]), timelineGrouping);
            rows.push({
                type: 'separator',
                dateKey,
                label: formatDateKey(dateKey, timelineGrouping),
                firstItemIndex: index,
            });

            let groupEnd = index + 1;
            while (
                groupEnd < localItems.length &&
                getDateKey(getTimelineTimestamp(localItems[groupEnd]), timelineGrouping) === dateKey
            ) {
                groupEnd += 1;
            }

            let rowStart = index;
            while (rowStart < groupEnd) {
                const count = Math.min(columnCount, groupEnd - rowStart);
                rows.push({
                    type: 'items',
                    dateKey,
                    startIndex: rowStart,
                    count,
                });
                rowStart += count;
            }

            index = groupEnd;
        }

        return rows;
    }, [localItems, timelineGrouping, columnCount]);

    // Stable identity: VirtualGrid depends on this in an effect to break the
    // no-overflow deadlock, and a fresh function each render would re-run it.
    const handleScroll = useCallback((scrollTop: number, clientHeight: number, scrollHeight: number, visibleItemIndex?: number) => {
        if (!hasNextPage || isNextPageLoading) return;

        // Load more when near bottom (e.g. 2 screens away)
        if (scrollHeight - (scrollTop + clientHeight) < clientHeight * 2) {
            loadNextPage(localItems.length, localItems.length + 20);
        }

        // Calculate current visible item for date header
        if (localItems.length > 0) {
            const index = Math.max(0, Math.min(visibleItemIndex ?? 0, localItems.length - 1));
            const visibleItem = localItems[index];
            if (visibleItem) {
                const dateKey = getDateKey(getTimelineTimestamp(visibleItem), timelineGrouping);
                const formatted = formatDateKey(dateKey, timelineGrouping);
                setCurrentDateHeader(formatted);
                setShowDateHeader(true);

                // Hide header after scrolling stops
                if (hideHeaderTimeout.current) {
                    clearTimeout(hideHeaderTimeout.current);
                }
                hideHeaderTimeout.current = setTimeout(() => {
                    setShowDateHeader(false);
                }, 1500);
            }
        }
    }, [hasNextPage, isNextPageLoading, loadNextPage, localItems, timelineGrouping]);

    return (
        <div ref={containerRef} className="w-full h-full flex-1 min-h-0 overflow-hidden bg-background relative">
            {/* Floating Date Header */}
            {currentDateHeader && (
                <div
                    className={`absolute top-4 left-1/2 -translate-x-1/2 z-50 px-4 py-2 rounded-full bg-background/80 backdrop-blur-sm border shadow-lg flex items-center gap-2 transition-opacity duration-300 ${showDateHeader ? 'opacity-100' : 'opacity-0 pointer-events-none'
                        }`}
                >
                    <Calendar className="h-4 w-4 text-muted-foreground" />
                    <span className="text-sm font-medium">{currentDateHeader}</span>
                </div>
            )}

            {width > 0 && height > 0 ? (
                <VirtualGrid
                    items={localItems || []}
                    rows={displayRows}
                    columnCount={columnCount}
                    columnWidth={columnWidth}
                    gap={GAP}
                    height={height}
                    width={width}
                    isNextPageLoading={isNextPageLoading}
                    onScroll={handleScroll}
                    ItemWrapper={ItemWrapper}
                    contextMenuExtras={contextMenuExtras}
                    albums={albums}
                    onAddToAlbum={handleAddToAlbum}
                    onItemClick={onItemClick}
                    onToggleFavorite={handleToggleFavorite}
                    onSetRating={handleSetRating}
                    onDelete={handleDelete}
                    onArchive={handleArchive}
                    onUnarchive={handleUnarchive}
                    onRemoveLocalCopy={handleRemoveLocalCopy}
                    onDownloadLocalCopy={handleDownloadLocalCopy}
                    theme={theme}
                />
            ) : (
                <div className="grid grid-cols-2 md:grid-cols-4 lg:grid-cols-6 gap-4 p-4">
                    {Array.from({ length: 12 }).map((_, i) => (
                        <Skeleton key={i} className="aspect-square w-full rounded-xl" />
                    ))}
                </div>
            )}
        </div>
    );
}
