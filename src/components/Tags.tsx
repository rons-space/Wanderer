import { useState, useEffect } from "react";
import { useLatestRequest } from "@/hooks/use-latest-request";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/lib/api";
import { PLACEHOLDER_SRC, handleImageError } from "@/lib/placeholder";
import { MediaItem, Tag as TagData } from "@/types";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { toast } from "sonner";
import { Tag as TagIcon, ChevronLeft, Hash } from "lucide-react";

export function Tags() {
    const [tags, setTags] = useState<TagData[]>([]);
    const [isLoading, setIsLoading] = useState(true);
    const [selectedTag, setSelectedTag] = useState<string | null>(null);
    const [tagMedia, setTagMedia] = useState<MediaItem[]>([]);
    const beginTagRequest = useLatestRequest();

    const loadTags = async () => {
        setIsLoading(true);
        try {
            const data = await api.getAllTags();
            setTags(data);
        } catch (e) {
            console.error("Failed to load tags:", e);
            toast.error("Failed to load tags");
        } finally {
            setIsLoading(false);
        }
    };

    useEffect(() => {
        loadTags();
    }, []);

    const handleSelectTag = async (tag: string) => {
        setSelectedTag(tag);
        // Clicking through tags faster than they load used to leave whichever
        // request finished last on screen, under whichever tag was selected last.
        const isCurrent = beginTagRequest();
        try {
            const media = await api.getMediaByTag(tag, 100, 0);
            if (!isCurrent()) return;
            setTagMedia(media);
        } catch (e) {
            if (!isCurrent()) return;
            console.error("Failed to load photos for this tag:", e);
            toast.error("Failed to load photos for this tag");
        }
    };

    const handleBack = () => {
        setSelectedTag(null);
        setTagMedia([]);
    };

    if (isLoading) {
        return (
            <div className="h-full w-full flex items-center justify-center">
                <div className="text-muted-foreground">Loading tags...</div>
            </div>
        );
    }

    // Show tag detail view
    if (selectedTag) {
        return (
            <div className="h-full w-full flex flex-col">
                <div className="flex items-center gap-4 p-4 border-b">
                    <Button variant="ghost" size="icon" onClick={handleBack}>
                        <ChevronLeft className="h-5 w-5" />
                    </Button>
                    <div className="flex items-center gap-3">
                        <div className="w-10 h-10 rounded-full bg-primary/10 flex items-center justify-center">
                            <Hash className="h-5 w-5 text-primary" />
                        </div>
                        <div>
                            <h1 className="text-lg font-semibold capitalize">{selectedTag}</h1>
                            <p className="text-sm text-muted-foreground">
                                {tagMedia.length} photos
                            </p>
                        </div>
                    </div>
                </div>
                <ScrollArea className="flex-1 p-4">
                    <div className="grid grid-cols-4 sm:grid-cols-6 md:grid-cols-8 gap-2">
                        {tagMedia.map((item) => (
                            <div
                                key={item.id}
                                className="aspect-square rounded-lg overflow-hidden bg-muted"
                            >
                                <img
                                    src={
                                        item.thumbnail_path
                                            ? convertFileSrc(item.thumbnail_path)
                                            : PLACEHOLDER_SRC
                                    }
                                    alt=""
                                    className="w-full h-full object-cover"
                                    onError={handleImageError}
                                />
                            </div>
                        ))}
                    </div>
                </ScrollArea>
            </div>
        );
    }

    // Show tags grid
    if (tags.length === 0) {
        return (
            <div className="h-full w-full flex flex-col items-center justify-center gap-4">
                <div className="text-4xl">🏷️</div>
                <h2 className="text-xl font-semibold">No tags detected yet</h2>
                <p className="text-muted-foreground text-center max-w-md">
                    AI object detection will automatically tag your photos.
                    Tags will appear here once photos are scanned.
                </p>
            </div>
        );
    }

    return (
        <div className="h-full w-full flex flex-col">
            <div className="flex items-center justify-between p-4 border-b">
                <div className="flex items-center gap-2">
                    <TagIcon className="h-5 w-5" />
                    <h1 className="text-lg font-semibold">Tags</h1>
                    <span className="text-sm text-muted-foreground">
                        ({tags.length})
                    </span>
                </div>
            </div>
            <ScrollArea className="flex-1 p-4">
                <div className="flex flex-wrap gap-2">
                    {tags.map((tag) => (
                        <Badge
                            key={tag.name}
                            variant="secondary"
                            className="cursor-pointer hover:bg-primary hover:text-primary-foreground transition-colors text-sm py-2 px-3"
                            onClick={() => handleSelectTag(tag.name)}
                        >
                            <Hash className="h-3 w-3 mr-1" />
                            {tag.name}
                            <span className="ml-2 text-xs opacity-70">{tag.media_count}</span>
                        </Badge>
                    ))}
                </div>
            </ScrollArea>
        </div>
    );
}
