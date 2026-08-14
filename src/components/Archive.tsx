import { useCallback } from "react";
import { Archive as ArchiveIcon } from "lucide-react";
import { api } from "@/lib/api";
import { MediaListView } from "./MediaListView";

export function Archive() {
    const fetcher = useCallback(
        (limit: number, offset: number) => api.getArchivedMedia(limit, offset),
        [],
    );

    return (
        <MediaListView
            fetcher={fetcher}
            title="Archive"
            icon={ArchiveIcon}
            iconClassName="w-5 h-5 text-orange-500"
            iconWrapperClassName="bg-orange-500/10"
            emptyTitle="No archived items"
            emptyDescription="Archived items are hidden from your main timeline but appear here."
        />
    );
}
