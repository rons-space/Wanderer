import { useState, useEffect, useCallback } from "react";
import { Album, MediaItem } from "../types";
import { api } from "../lib/api";
import { useMediaActions } from "@/hooks/use-media-actions";
import { MediaGrid } from "./MediaGrid";
import { Button } from "./ui/button";
import { ArrowLeft } from "lucide-react";

interface AlbumDetailProps {
    album: Album;
    onBack: () => void;
}

export function AlbumDetail({ album, onBack }: AlbumDetailProps) {
    const [items, setItems] = useState<MediaItem[]>([]);
    // The grid mutates items through their owner rather than a copy of its own.
    const updateItems = useCallback(
        (updater: (current: MediaItem[]) => MediaItem[]) => setItems(updater),
        [],
    );
    const actions = useMediaActions(updateItems);
    const [hasNextPage, setHasNextPage] = useState(true);
    const [isNextPageLoading, setIsNextPageLoading] = useState(false);

    useEffect(() => {
        const loadInitial = async () => {
            setIsNextPageLoading(true);
            try {
                const initialItems = await api.getAlbumMedia(album.id, 20, 0);
                setItems(initialItems);
                if (initialItems.length < 20) {
                    setHasNextPage(false);
                }
            } catch (e) {
                console.error("Failed to load album media", e);
            } finally {
                setIsNextPageLoading(false);
            }
        };
        loadInitial();
    }, [album.id]);

    const loadNextPage = async (startIndex: number, stopIndex: number) => {
        if (isNextPageLoading) return;
        setIsNextPageLoading(true);
        try {
            const limit = stopIndex - startIndex + 20;
            const offset = startIndex;
            const newItems = await api.getAlbumMedia(album.id, limit, offset);

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
            console.error("Failed to load album media", error);
        } finally {
            setIsNextPageLoading(false);
        }
    };

    return (
        <div className="h-full w-full flex flex-col">
            <div className="flex items-center gap-2 p-4 border-b">
                <Button variant="ghost" size="icon" onClick={onBack}>
                    <ArrowLeft className="h-4 w-4" />
                </Button>
                <h1 className="text-xl font-bold">{album.name}</h1>
                <span className="text-muted-foreground ml-auto text-sm">
                    {new Date(album.created_at * 1000).toLocaleDateString()}
                </span>
            </div>

            <div className="flex-1 overflow-hidden">
                <MediaGrid
                    items={items}
                    hasNextPage={hasNextPage}
                    isNextPageLoading={isNextPageLoading}
                    loadNextPage={loadNextPage}
                    actions={actions}
                />
            </div>
        </div>
    );
}
