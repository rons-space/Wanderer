import { useCallback, useState } from "react";
import { RotateCcw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/lib/api";
import { MediaItem } from "@/types";
import { MediaListView, type MediaListControls } from "./MediaListView";
import { ContextMenuItem, ContextMenuSeparator } from "@/components/ui/context-menu";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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
    const [deleteFromTelegram, setDeleteFromTelegram] = useState(false);
    const [isEmptying, setIsEmptying] = useState(false);

    const fetcher = useCallback((limit: number, offset: number) => api.getTrash(limit, offset), []);

    const handleRestore = useCallback(async (item: MediaItem, controls: MediaListControls) => {
        try {
            await api.restoreFromTrash(item.id);
            controls.drop(item.id);
            toast.success("Restored from trash");
        } catch (e) {
            console.error("Failed to restore:", e);
            toast.error("Failed to restore item");
        }
    }, []);

    const handleEmptyTrash = useCallback(
        async (controls: MediaListControls) => {
            setIsEmptying(true);
            try {
                const count = await api.emptyTrash(deleteFromTelegram);
                controls.clear();
                toast.success(`Permanently deleted ${count} item${count !== 1 ? "s" : ""}`);
            } catch (e) {
                console.error("Failed to empty trash:", e);
                toast.error("Failed to empty trash");
            } finally {
                setIsEmptying(false);
            }
        },
        [deleteFromTelegram],
    );

    // Restore is contributed to the grid's own context menu. Wrapping the cell in a
    // second ContextMenu nested the two triggers, and the inner menu won, so the
    // entry was unreachable.
    const contextMenuExtras = useCallback(
        (item: MediaItem, controls: MediaListControls) => (
            <>
                <ContextMenuItem onClick={() => handleRestore(item, controls)}>
                    <RotateCcw className="mr-2 h-4 w-4" aria-hidden="true" />
                    Restore
                </ContextMenuItem>
                <ContextMenuSeparator />
            </>
        ),
        [handleRestore],
    );

    const headerActions = useCallback(
        (controls: MediaListControls) => {
            const count = controls.items.length;
            if (count === 0) return null;

            return (
                <AlertDialog>
                    <AlertDialogTrigger asChild>
                        <Button variant="destructive" size="sm">
                            <Trash2 className="mr-2 h-4 w-4" aria-hidden="true" />
                            Empty Trash
                        </Button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>Empty Trash?</AlertDialogTitle>
                            <AlertDialogDescription>
                                This will permanently delete {count} item{count !== 1 ? "s" : ""}{" "}
                                from your device. This action cannot be undone.
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
                                onClick={() => handleEmptyTrash(controls)}
                                disabled={isEmptying}
                                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                            >
                                {isEmptying ? "Deleting..." : "Delete Permanently"}
                            </AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            );
        },
        [deleteFromTelegram, handleEmptyTrash, isEmptying],
    );

    return (
        <MediaListView
            fetcher={fetcher}
            title="Trash"
            icon={Trash2}
            iconClassName="w-5 h-5 text-destructive"
            iconWrapperClassName="bg-destructive/10"
            subtitle={(count) =>
                `${count} ${count === 1 ? "item" : "items"} • Items are permanently deleted after 30 days`
            }
            headerActions={headerActions}
            contextMenuExtras={contextMenuExtras}
            emptyTitle="Trash is empty"
            emptyDescription="Deleted items will appear here"
        />
    );
}
