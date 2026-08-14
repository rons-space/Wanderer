import { useCallback } from "react";
import { Heart } from "lucide-react";
import { api } from "@/lib/api";
import { MediaListView } from "./MediaListView";

export function Favorites() {
    const fetcher = useCallback(
        (limit: number, offset: number) => api.getFavorites(limit, offset),
        [],
    );

    return (
        <MediaListView
            fetcher={fetcher}
            title="Favorites"
            icon={Heart}
            iconClassName="w-5 h-5 fill-red-500 text-red-500"
            iconWrapperClassName="bg-red-500/10"
            emptyTitle="No favorites yet"
            emptyDescription="Click the heart icon on any photo to add it to your favorites"
        />
    );
}
