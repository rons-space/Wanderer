import { useState, useEffect } from "react";
import { api } from "@/lib/api";
import { Album } from "@/types";
import { Button } from "@/components/ui/button";
import {
    Select,
    SelectContent,
    SelectItem,
    SelectTrigger,
} from "@/components/ui/select";
import { Heart, Trash2, FolderPlus, X, CheckSquare, Download, Cloud } from "lucide-react";
import { toast } from "sonner";
import { open } from "@tauri-apps/plugin-dialog";

interface BulkActionBarProps {
    selectedIds: Set<number>;
    onClearSelection: () => void;
    onActionComplete: () => void;
}

export function BulkActionBar({ selectedIds, onClearSelection, onActionComplete }: BulkActionBarProps) {
    const [albums, setAlbums] = useState<Album[]>([]);
    const [isLoading, setIsLoading] = useState(false);

    useEffect(() => {
        api.getAlbums().then(setAlbums).catch(console.error);
    }, []);

    const count = selectedIds.size;
    if (count === 0) return null;

    const handleBulkFavorite = async () => {
        setIsLoading(true);
        try {
            const ids = Array.from(selectedIds);
            const updated = await api.bulkSetFavorite(ids, true);
            toast.success(`Added ${updated} items to favorites`);
            onClearSelection();
            onActionComplete();
        } catch (e) {
            console.error(e);
            toast.error("Failed to favorite items");
        } finally {
            setIsLoading(false);
        }
    };

    const handleBulkDelete = async () => {
        setIsLoading(true);
        try {
            const ids = Array.from(selectedIds);
            const deleted = await api.bulkDelete(ids);
            toast.success(`Moved ${deleted} items to trash`);
            onClearSelection();
            onActionComplete();
        } catch (e) {
            console.error(e);
            toast.error("Failed to delete items");
        } finally {
            setIsLoading(false);
        }
    };

    const handleAddToAlbum = async (albumId: string) => {
        if (!albumId || albumId === "__none__") return;
        setIsLoading(true);
        try {
            const ids = Array.from(selectedIds);
            const added = await api.bulkAddToAlbum(parseInt(albumId), ids);
            toast.success(`Added ${added} items to album`);
            onClearSelection();
            onActionComplete();
        } catch (e) {
            console.error(e);
            toast.error("Failed to add to album");
        } finally {
            setIsLoading(false);
        }
    };

    const handleExport = async () => {
        setIsLoading(true);
        try {
            // Open folder picker dialog
            const destination = await open({
                directory: true,
                multiple: false,
                title: "Select Export Destination",
            });

            if (!destination) {
                setIsLoading(false);
                return; // User cancelled
            }

            const ids = Array.from(selectedIds);
            const exported = await api.exportMedia(ids, destination as string);
            if (exported > 0) {
                toast.success(`Exported ${exported} items to folder`);
                onClearSelection();
            } else {
                toast.error("Export failed: selected files unavailable locally and in cloud");
            }
        } catch (e) {
            console.error(e);
            toast.error("Failed to export items");
        } finally {
            setIsLoading(false);
        }
    };

    const handleBulkRemoveLocalCopy = async () => {
        const ids = Array.from(selectedIds);
        if (ids.length === 0) return;

        if (!confirm(`Remove local copy for ${ids.length} selected item(s)? They will remain available in cloud.`)) {
            return;
        }

        setIsLoading(true);
        try {
            let success = 0;
            let failed = 0;

            for (const mediaId of ids) {
                try {
                    await api.removeLocalCopy(mediaId);
                    success += 1;
                } catch (e) {
                    failed += 1;
                    console.error(`Failed to remove local copy for media ${mediaId}:`, e);
                }
            }

            if (success > 0) {
                toast.success(`Removed local copies for ${success} item${success > 1 ? "s" : ""}`);
                onClearSelection();
                onActionComplete();
            }

            if (failed > 0) {
                toast.warning(`${failed} item${failed > 1 ? "s" : ""} skipped (not uploaded yet or already cloud-only)`);
            }

            if (success === 0 && failed > 0) {
                toast.error("No items were updated");
            }
        } finally {
            setIsLoading(false);
        }
    };

    return (
        <div className="fixed bottom-6 left-1/2 -translate-x-1/2 z-50 animate-in slide-in-from-bottom-4 duration-300">
            <div className="flex items-center gap-1 px-3 py-2 bg-zinc-900 text-white rounded-xl shadow-2xl border border-zinc-700">
                {/* Selection count */}
                <div className="flex items-center gap-2 px-3 py-1.5 bg-blue-600 rounded-lg mr-2">
                    <CheckSquare className="h-4 w-4" />
                    <span className="font-semibold text-sm">{count}</span>
                </div>

                {/* Actions */}
                <Button
                    variant="ghost"
                    size="sm"
                    onClick={handleBulkFavorite}
                    disabled={isLoading}
                    className="text-white hover:bg-zinc-700 gap-1.5"
                >
                    <Heart className="h-4 w-4" />
                    <span className="hidden sm:inline">Favorite</span>
                </Button>

                <Select onValueChange={handleAddToAlbum} disabled={isLoading}>
                    <SelectTrigger className="h-8 w-auto min-w-[100px] border-0 bg-transparent text-white hover:bg-zinc-700 gap-1.5 [&>svg]:hidden">
                        <FolderPlus className="h-4 w-4 shrink-0" />
                        <span className="hidden sm:inline text-sm">Album</span>
                    </SelectTrigger>
                    <SelectContent>
                        {albums.length === 0 ? (
                            <SelectItem value="__none__" disabled>No albums</SelectItem>
                        ) : (
                            albums.map((album) => (
                                <SelectItem key={album.id} value={album.id.toString()}>
                                    {album.name}
                                </SelectItem>
                            ))
                        )}
                    </SelectContent>
                </Select>

                <Button
                    variant="ghost"
                    size="sm"
                    onClick={handleExport}
                    disabled={isLoading}
                    className="text-white hover:bg-zinc-700 gap-1.5"
                >
                    <Download className="h-4 w-4" />
                    <span className="hidden sm:inline">Export</span>
                </Button>

                <Button
                    variant="ghost"
                    size="sm"
                    onClick={handleBulkRemoveLocalCopy}
                    disabled={isLoading}
                    className="text-white hover:bg-zinc-700 gap-1.5"
                >
                    <Cloud className="h-4 w-4" />
                    <span className="hidden sm:inline">Cloud Only</span>
                </Button>

                <div className="w-px h-6 bg-zinc-600 mx-1" />

                <Button
                    variant="ghost"
                    size="sm"
                    onClick={handleBulkDelete}
                    disabled={isLoading}
                    className="text-red-400 hover:bg-red-500/20 hover:text-red-300 gap-1.5"
                >
                    <Trash2 className="h-4 w-4" />
                    <span className="hidden sm:inline">Delete</span>
                </Button>

                {/* Clear selection */}
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Clear selection"
                    onClick={onClearSelection}
                    className="ml-1 text-zinc-400 hover:text-white hover:bg-zinc-700 h-8 w-8 rounded-lg"
                >
                    <X className="h-4 w-4" />
                </Button>
            </div>
        </div>
    );
}

