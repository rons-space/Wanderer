import { useState, useEffect, useCallback } from "react";
import { api } from "@/lib/api";
import { MediaItem } from "@/types";
import { MediaGrid } from "./MediaGrid";
import { MediaViewer } from "./MediaViewer";
import { Trash2, RotateCcw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { ContextMenuItem, ContextMenuSeparator } from "@/components/ui/context-menu";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
    AlertDialogTrigger,
} from "@/components/ui/alert-dialog";

export function Trash() {
    const [items, setItems] = useState<MediaItem[]>([]);
    const [selectedItem, setSelectedItem] = useState<MediaItem | null>(null);
    const [hasNextPage, setHasNextPage] = useState(true);
    const [isNextPageLoading, setIsNextPageLoading] = useState(false);
    const [deleteFromTelegram, setDeleteFromTelegram] = useState(false);
    const [isEmptying, setIsEmptying] = useState(false);

    const loadItems = useCallback(async () => {
        try {
            const newItems = await api.getTrash(100, 0);
            setItems(newItems);
            setHasNextPage(newItems.length >= 100);
        } catch (e) {
            console.error("Failed to load trash:", e);
        }
    }, []);

    useEffect(() => {
        loadItems();
    }, [loadItems]);

    const loadNextPage = async (start: number, stop: number) => {
        if (isNextPageLoading || !hasNextPage) return;

        setIsNextPageLoading(true);
        try {
            const newItems = await api.getTrash(stop - start, start);
            if (newItems.length === 0) {
                setHasNextPage(false);
            } else {
                setItems(prev => [...prev, ...newItems]);
                setHasNextPage(newItems.length >= (stop - start));
            }
        } catch (e) {
            console.error("Failed to load more trash items:", e);
        } finally {
            setIsNextPageLoading(false);
        }
    };

    const handleRestore = useCallback(async (item: MediaItem) => {
        try {
            await api.restoreFromTrash(item.id);
            setItems(prev => prev.filter(i => i.id !== item.id));
            toast.success("Restored from trash");
        } catch (e) {
            console.error("Failed to restore:", e);
            toast.error("Failed to restore item");
        }
    }, []);

    const handleEmptyTrash = async () => {
        setIsEmptying(true);
        try {
            const count = await api.emptyTrash(deleteFromTelegram);
            setItems([]);
            toast.success(`Permanently deleted ${count} item${count !== 1 ? 's' : ''}`);
        } catch (e) {
            console.error("Failed to empty trash:", e);
            toast.error("Failed to empty trash");
        } finally {
            setIsEmptying(false);
        }
    };

    const handleItemClick = (item: MediaItem) => {
        setSelectedItem(item);
    };

    // Restore is contributed to the grid's own context menu. Wrapping the cell
    // in a second ContextMenu nested the two triggers, and the inner menu won,
    // so the entry was unreachable.
    const contextMenuExtras = useCallback((item: MediaItem) => (
        <>
            <ContextMenuItem onClick={() => handleRestore(item)}>
                <RotateCcw className="mr-2 h-4 w-4" />
                Restore
            </ContextMenuItem>
            <ContextMenuSeparator />
        </>
    ), [handleRestore]);

    return (
        <div className="flex flex-col h-full">
            {/* Header */}
            <div className="flex items-center justify-between p-4 border-b">
                <div className="flex items-center gap-3">
                    <div className="flex items-center justify-center w-10 h-10 rounded-full bg-destructive/10">
                        <Trash2 className="w-5 h-5 text-destructive" />
                    </div>
                    <div>
                        <h1 className="text-lg font-semibold">Trash</h1>
                        <p className="text-sm text-muted-foreground">
                            {items.length} {items.length === 1 ? 'item' : 'items'} • Items are permanently deleted after 30 days
                        </p>
                    </div>
                </div>

                {/* Empty Trash Button */}
                {items.length > 0 && (
                    <AlertDialog>
                        <AlertDialogTrigger asChild>
                            <Button variant="destructive" size="sm">
                                <Trash2 className="mr-2 h-4 w-4" />
                                Empty Trash
                            </Button>
                        </AlertDialogTrigger>
                        <AlertDialogContent>
                            <AlertDialogHeader>
                                <AlertDialogTitle>Empty Trash?</AlertDialogTitle>
                                <AlertDialogDescription>
                                    This will permanently delete {items.length} item{items.length !== 1 ? 's' : ''} from your device.
                                    This action cannot be undone.
                                </AlertDialogDescription>
                            </AlertDialogHeader>
                            <div className="flex items-center space-x-2 py-4">
                                <Checkbox
                                    id="delete-telegram"
                                    checked={deleteFromTelegram}
                                    onCheckedChange={(checked) => setDeleteFromTelegram(checked === true)}
                                />
                                <label
                                    htmlFor="delete-telegram"
                                    className="text-sm text-muted-foreground cursor-pointer"
                                >
                                    Also delete from Telegram Saved Messages
                                </label>
                            </div>
                            <AlertDialogFooter>
                                <AlertDialogCancel>Cancel</AlertDialogCancel>
                                <AlertDialogAction
                                    onClick={handleEmptyTrash}
                                    disabled={isEmptying}
                                    className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                                >
                                    {isEmptying ? "Deleting..." : "Delete Permanently"}
                                </AlertDialogAction>
                            </AlertDialogFooter>
                        </AlertDialogContent>
                    </AlertDialog>
                )}
            </div>

            {/* Grid */}
            {items.length === 0 ? (
                <div className="flex-1 flex items-center justify-center">
                    <div className="text-center">
                        <Trash2 className="w-16 h-16 mx-auto mb-4 text-muted-foreground/30" />
                        <h2 className="text-lg font-medium text-muted-foreground">Trash is empty</h2>
                        <p className="text-sm text-muted-foreground/60">
                            Deleted items will appear here
                        </p>
                    </div>
                </div>
            ) : (
                <MediaGrid
                    items={items}
                    hasNextPage={hasNextPage}
                    isNextPageLoading={isNextPageLoading}
                    loadNextPage={loadNextPage}
                    contextMenuExtras={contextMenuExtras}
                    onItemClick={handleItemClick}
                    onItemsChange={loadItems}
                />
            )}

            {/* Media Viewer Modal */}
            {selectedItem && (
                <MediaViewer
                    item={selectedItem}
                    open={!!selectedItem}
                    onClose={() => setSelectedItem(null)}
                />
            )}
        </div>
    );
}

