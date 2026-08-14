import { useState, useEffect, useCallback } from "react";
import { convertFileSrc } from '@tauri-apps/api/core';
import { PLACEHOLDER_SRC, handleImageError } from '@/lib/placeholder';
import { api } from "@/lib/api";
import { MediaItem } from "@/types";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { MediaViewer } from "./MediaViewer";
import { toast } from "sonner";
import { Video, Clock, Star, ChevronLeft, Sparkles } from "lucide-react";

interface SmartAlbumCounts {
    videos: number;
    recent: number;
    top_rated: number;
}

type SmartAlbumType = "videos" | "recent" | "top_rated";

const ALBUM_INFO: Record<SmartAlbumType, { title: string; icon: React.ElementType; color: string; description: string }> = {
    videos: {
        title: "Videos",
        icon: Video,
        color: "text-purple-500 bg-purple-500/10",
        description: "All your video clips",
    },
    recent: {
        title: "Recent",
        icon: Clock,
        color: "text-blue-500 bg-blue-500/10",
        description: "Last 30 days",
    },
    top_rated: {
        title: "Top Rated",
        icon: Star,
        color: "text-amber-500 bg-amber-500/10",
        description: "4+ star photos",
    },
};

export function SmartAlbums() {
    const [counts, setCounts] = useState<SmartAlbumCounts>({ videos: 0, recent: 0, top_rated: 0 });
    const [isLoading, setIsLoading] = useState(true);
    const [selectedAlbum, setSelectedAlbum] = useState<SmartAlbumType | null>(null);
    const [albumItems, setAlbumItems] = useState<MediaItem[]>([]);
    const [selectedItem, setSelectedItem] = useState<MediaItem | null>(null);

    const loadCounts = useCallback(async () => {
        try {
            const data = await api.getSmartAlbumCounts();
            setCounts(data);
        } catch (e) {
            console.error("Failed to load smart album counts:", e);
            toast.error("Failed to load smart albums");
        } finally {
            setIsLoading(false);
        }
    }, []);

    useEffect(() => {
        loadCounts();
    }, [loadCounts]);

    const loadAlbum = async (type: SmartAlbumType) => {
        setSelectedAlbum(type);
        try {
            let items: MediaItem[];
            switch (type) {
                case "videos":
                    items = await api.getVideos(100, 0);
                    break;
                case "recent":
                    items = await api.getRecent(100, 0);
                    break;
                case "top_rated":
                    items = await api.getTopRated(100, 0);
                    break;
            }
            setAlbumItems(items);
        } catch (e) {
            console.error("Failed to load album contents:", e);
            toast.error("Failed to load album contents");
        }
    };

    const handleBack = () => {
        setSelectedAlbum(null);
        setAlbumItems([]);
    };

    if (isLoading) {
        return (
            <div className="h-full w-full flex items-center justify-center">
                <div className="text-muted-foreground">Loading smart albums...</div>
            </div>
        );
    }

    // Album detail view
    if (selectedAlbum) {
        const info = ALBUM_INFO[selectedAlbum];
        const Icon = info.icon;

        return (
            <div className="h-full w-full flex flex-col">
                <div className="flex items-center gap-4 p-4 border-b">
                    <Button variant="ghost" size="icon" onClick={handleBack}>
                        <ChevronLeft className="h-5 w-5" />
                    </Button>
                    <div className="flex items-center gap-3">
                        <div className={`w-10 h-10 rounded-full flex items-center justify-center ${info.color}`}>
                            <Icon className="h-5 w-5" />
                        </div>
                        <div>
                            <h1 className="text-lg font-semibold">{info.title}</h1>
                            <p className="text-sm text-muted-foreground">
                                {albumItems.length} items
                            </p>
                        </div>
                    </div>
                </div>
                <ScrollArea className="flex-1">
                    <div className="p-4">
                        {albumItems.length === 0 ? (
                            <div className="flex flex-col items-center justify-center h-32">
                                <p className="text-muted-foreground">No items in this album</p>
                            </div>
                        ) : (
                            <div className="grid grid-cols-4 sm:grid-cols-6 md:grid-cols-8 gap-2">
                                {albumItems.map((item) => (
                                    <div
                                        key={item.id}
                                        className="aspect-square rounded-lg overflow-hidden bg-muted cursor-pointer hover:ring-2 hover:ring-primary transition-all"
                                        onClick={() => setSelectedItem(item)}
                                    >
                                        {item.thumbnail_path ? (
                                            <img
                                                src={convertFileSrc(item.thumbnail_path)}
                                                alt=""
                                                className="w-full h-full object-cover"
                                                onError={handleImageError}
                                            />
                                        ) : item.mime_type?.startsWith('video/') ? (
                                            <div className="w-full h-full flex items-center justify-center bg-muted">
                                                <Video className="h-8 w-8 text-muted-foreground" />
                                            </div>
                                        ) : (
                                            <img
                                                src={PLACEHOLDER_SRC}
                                                alt=""
                                                className="w-full h-full object-cover"
                                            />
                                        )}
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>
                </ScrollArea>

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

    // Smart Albums grid
    return (
        <div className="h-full w-full flex flex-col">
            <div className="flex items-center gap-2 p-4 border-b">
                <Sparkles className="h-5 w-5 text-primary" />
                <h1 className="text-lg font-semibold">Smart Albums</h1>
            </div>
            <ScrollArea className="flex-1">
                <div className="p-4">
                    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
                        {(Object.keys(ALBUM_INFO) as SmartAlbumType[]).map((type) => {
                            const info = ALBUM_INFO[type];
                            const Icon = info.icon;
                            const count = counts[type];

                            return (
                                <Card
                                    key={type}
                                    className="cursor-pointer hover:ring-2 hover:ring-primary transition-all"
                                    onClick={() => loadAlbum(type)}
                                >
                                    <CardContent className="p-6">
                                        <div className="flex items-center gap-4">
                                            <div className={`w-12 h-12 rounded-xl flex items-center justify-center ${info.color}`}>
                                                <Icon className="h-6 w-6" />
                                            </div>
                                            <div className="flex-1">
                                                <h3 className="font-semibold">{info.title}</h3>
                                                <p className="text-sm text-muted-foreground">{info.description}</p>
                                            </div>
                                            <Badge variant="secondary" className="text-lg px-3 py-1">
                                                {count}
                                            </Badge>
                                        </div>
                                    </CardContent>
                                </Card>
                            );
                        })}
                    </div>
                </div>
            </ScrollArea>
        </div>
    );
}
