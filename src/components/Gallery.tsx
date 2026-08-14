import { useState, useEffect, useCallback, useContext, useMemo, useRef, createContext } from "react";
import { MediaItem } from "../types";
import { api } from "../lib/api";
import { useMediaActions } from "@/hooks/use-media-actions";
import { subscribe } from "@/lib/events";
import { toast } from "sonner";
import { MediaGrid } from "./MediaGrid";
import { BulkActionBar } from "./BulkActionBar";
import { MediaViewer } from "./MediaViewer";
import { useTheme } from "@/contexts/ThemeContext";
import { cn } from "@/lib/utils";

// A maximised window shows far more than twenty thumbnails, and a first page
// that does not overflow the viewport never produces the scroll event that asks
// for the second one. MediaGrid also nudges when the content does not overflow,
// but starting with a screenful keeps the common case to a single round trip.
const PAGE_SIZE = 60;

// The backend clamps a page at MAX_PAGE_SIZE, so a refresh of a very deep scroll
// position is capped rather than silently truncated by the database layer.
const MAX_REFRESH = 1000;

/**
 * Selection state reaches the grid cells through context rather than through a
 * closure. The wrapper below is passed to `MediaGrid` as a component type, and
 * a component defined inside `Gallery` would be a brand new type on every
 * render, which unmounts and remounts every visible cell (and re-downloads
 * every thumbnail) each time the selection changed.
 */
interface SelectionState {
    isSelectionMode: boolean;
    selectedIds: Set<number>;
    toggle: (id: number) => void;
}

const SelectionContext = createContext<SelectionState>({
    isSelectionMode: false,
    selectedIds: new Set<number>(),
    toggle: () => { },
});

function SelectableItemWrapper({ item, children }: { item: MediaItem; children: React.ReactNode }) {
    const { isSelectionMode, selectedIds, toggle } = useContext(SelectionContext);
    const isSelected = selectedIds.has(item.id);

    return (
        <div
            className={cn(
                "relative transition-all duration-150 h-full w-full",
                isSelected && "ring-2 ring-blue-500 ring-offset-2 ring-offset-background rounded-lg scale-[0.97]",
                isSelectionMode && "cursor-pointer"
            )}
        >
            {/* Selection checkbox overlay */}
            {isSelectionMode && (
                <div
                    aria-hidden="true"
                    className={cn(
                        "absolute top-2 left-2 z-20 w-6 h-6 rounded-full border-2 transition-all flex items-center justify-center",
                        isSelected
                            ? "bg-blue-500 border-blue-500 shadow-lg"
                            : "bg-black/50 border-white/60 backdrop-blur-sm"
                    )}
                >
                    {isSelected && (
                        <svg className="w-4 h-4 text-white" viewBox="0 0 20 20" fill="currentColor">
                            <path fillRule="evenodd" d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z" clipRule="evenodd" />
                        </svg>
                    )}
                </div>
            )}
            {/*
                Swallows clicks so they toggle the selection instead of opening
                the viewer. Hidden from assistive technology on purpose: the
                cell underneath is already a focusable button whose Enter and
                Space handler runs the same toggle, so exposing this would just
                announce a second, unlabelled control over every photo.
            */}
            {isSelectionMode && (
                <div
                    aria-hidden="true"
                    className="absolute inset-0 z-10"
                    onClick={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        toggle(item.id);
                    }}
                />
            )}
            {children}
        </div>
    );
}

export function Gallery() {
    const [items, setItems] = useState<MediaItem[]>([]);
    // The grid mutates items through their owner rather than a copy of its own.
    const updateItems = useCallback(
        (updater: (current: MediaItem[]) => MediaItem[]) => setItems(updater),
        [],
    );
    const [hasNextPage, setHasNextPage] = useState(true);
    const [isNextPageLoading, setIsNextPageLoading] = useState(false);
    const { theme } = useTheme();

    // Viewer State
    const [viewerOpen, setViewerOpen] = useState(false);
    const [selectedMedia, setSelectedMedia] = useState<MediaItem | null>(null);

    // Selection State
    const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
    const [isSelectionMode, setIsSelectionMode] = useState(false);
    // Read by the click handler, which has to keep a stable identity so that the
    // memoised grid cells are not invalidated every time selection mode flips.
    const isSelectionModeRef = useRef(isSelectionMode);
    useEffect(() => {
        isSelectionModeRef.current = isSelectionMode;
    }, [isSelectionMode]);

    const loadNextPage = async (startIndex: number, stopIndex: number) => {
        if (isNextPageLoading) return;
        setIsNextPageLoading(true);
        try {
            const limit = stopIndex - startIndex + 20;
            const offset = startIndex;
            const newItems = await api.getMedia(limit, offset);

            if (newItems.length === 0) {
                setHasNextPage(false);
            } else {
                setItems(prev => {
                    const existingIds = new Set(prev.map(i => i.id));
                    const filtered = newItems.filter(i => !existingIds.has(i.id));
                    return [...prev, ...filtered];
                });
            }
        } catch (error) {
            console.error("Failed to load media", error);
            toast.error("Failed to load media items");
        } finally {
            setIsNextPageLoading(false);
        }
    };

    // Read through a ref so refreshing keeps a stable identity: it is handed to
    // the `media-added` listener, which is registered once for the lifetime of
    // the view.
    const loadedCountRef = useRef(0);

    useEffect(() => {
        loadedCountRef.current = items.length;
    }, [items]);

    const refreshItems = useCallback(async () => {
        try {
            // Refresh the window the user has already scrolled through. Resetting
            // to the first page discarded every page after it, which is what made
            // a background import jump the gallery back to the top.
            const loaded = Math.min(Math.max(loadedCountRef.current, PAGE_SIZE), MAX_REFRESH);
            const newItems = await api.getMedia(loaded, 0);
            setItems(newItems);
        } catch (e) {
            console.error("Failed to refresh:", e);
        }
    }, []);

    const actions = useMediaActions(updateItems, refreshItems);

    // Initial load, separate from the subscription so neither waits on the other.
    useEffect(() => {
        let cancelled = false;

        api.getMedia(PAGE_SIZE, 0)
            .then((initialItems) => {
                if (!cancelled) {
                    setItems(initialItems);
                }
            })
            .catch((e) => {
                console.error("Initial load failed:", e);
                toast.error("Failed to load gallery");
            });

        return () => {
            cancelled = true;
        };
    }, []);

    // Listen for new media events
    useEffect(() => subscribe('media-added', () => {
        refreshItems();
    }), [refreshItems]);

    // Handle keyboard shortcuts
    useEffect(() => {
        const handleKeyDown = (e: KeyboardEvent) => {
            // Escape: clear selection
            if (e.key === 'Escape' && isSelectionMode) {
                setSelectedIds(new Set());
                setIsSelectionMode(false);
            }
            // Ctrl+A: select all visible
            if (e.key === 'a' && (e.ctrlKey || e.metaKey) && isSelectionMode) {
                e.preventDefault();
                setSelectedIds(new Set(items.map(i => i.id)));
            }
        };

        window.addEventListener('keydown', handleKeyDown);
        return () => window.removeEventListener('keydown', handleKeyDown);
    }, [isSelectionMode, items]);

    /** Adds or removes one id, leaving selection mode when the last one goes. */
    const toggleSelection = useCallback((mediaId: number) => {
        setSelectedIds((prev) => {
            const next = new Set(prev);
            if (next.has(mediaId)) {
                next.delete(mediaId);
            } else {
                next.add(mediaId);
            }
            if (next.size === 0) {
                setIsSelectionMode(false);
            }
            return next;
        });
    }, []);

    const handleItemClick = useCallback(
        (item: MediaItem, e?: React.MouseEvent) => {
            // Shift-click or Ctrl-click enters selection mode on the first item.
            if (e && (e.shiftKey || e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                setIsSelectionMode(true);
                setSelectedIds((prev) => new Set(prev).add(item.id));
                return;
            }

            if (isSelectionModeRef.current) {
                toggleSelection(item.id);
                return;
            }

            setSelectedMedia(item);
            setViewerOpen(true);
        },
        [toggleSelection],
    );

    const selection = useMemo(
        () => ({ isSelectionMode, selectedIds, toggle: toggleSelection }),
        [isSelectionMode, selectedIds, toggleSelection],
    );

    const clearSelection = () => {
        setSelectedIds(new Set());
        setIsSelectionMode(false);
    };

    const handleActionComplete = () => {
        refreshItems();
    };

    return (
        <div className={cn(
            "h-full w-full bg-background",
            theme !== 'explorer' && "animate-fade-in"
        )}>
            <SelectionContext.Provider value={selection}>
                <MediaGrid
                    items={items}
                    hasNextPage={hasNextPage}
                    isNextPageLoading={isNextPageLoading}
                    loadNextPage={loadNextPage}
                    onItemClick={handleItemClick}
                    ItemWrapper={isSelectionMode ? SelectableItemWrapper : undefined}
                    actions={actions}
                />
            </SelectionContext.Provider>

            <MediaViewer
                open={viewerOpen}
                onClose={() => setViewerOpen(false)}
                item={selectedMedia}
                items={items}
                onNavigate={setSelectedMedia}
            />

            <BulkActionBar
                selectedIds={selectedIds}
                onClearSelection={clearSelection}
                onActionComplete={handleActionComplete}
            />
        </div>
    );
}
