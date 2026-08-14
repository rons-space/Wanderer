import { useState, useEffect, useCallback } from "react";
import { Person, MediaItem } from "../types";
import { api } from "../lib/api";
import { useMediaActions } from "@/hooks/use-media-actions";
import { MediaGrid } from "./MediaGrid";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { ArrowLeft, Edit2, Save, X } from "lucide-react";
import { MediaViewer } from "./MediaViewer";
import { toast } from "sonner";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "@/components/ui/dialog";

interface PersonDetailProps {
    person: Person;
    onBack: (shouldRefresh?: boolean) => void;
    onUpdate?: (updatedPerson: Person) => void;
}

export function PersonDetail({ person, onBack, onUpdate }: PersonDetailProps) {
    const [items, setItems] = useState<MediaItem[]>([]);
    // The grid mutates items through their owner rather than a copy of its own.
    const updateItems = useCallback(
        (updater: (current: MediaItem[]) => MediaItem[]) => setItems(updater),
        [],
    );
    const actions = useMediaActions(updateItems);
    const [hasNextPage, setHasNextPage] = useState(true);
    const [isNextPageLoading, setIsNextPageLoading] = useState(false);

    // Rename/Merge State
    const [isEditing, setIsEditing] = useState(false);
    const [newName, setNewName] = useState(person.name || "");
    const [mergeTarget, setMergeTarget] = useState<Person | null>(null);
    const [isMergeDialogOpen, setIsMergeDialogOpen] = useState(false);
    const [isSavingName, setIsSavingName] = useState(false);

    // Viewer State
    const [selectedMedia, setSelectedMedia] = useState<MediaItem | null>(null);
    const [isViewerOpen, setIsViewerOpen] = useState(false);

    useEffect(() => {
        const loadInitial = async () => {
            setIsNextPageLoading(true);
            try {
                // Fetch first 50 items for the person
                const initialItems = await api.getMediaByPerson(person.id, 50, 0);
                setItems(initialItems);
                if (initialItems.length < 50) {
                    setHasNextPage(false);
                }
            } catch (e) {
                console.error("Failed to load person media", e);
            } finally {
                setIsNextPageLoading(false);
            }
        };
        loadInitial();
    }, [person.id]);

    const loadNextPage = async (startIndex: number, stopIndex: number) => {
        if (isNextPageLoading) return;
        setIsNextPageLoading(true);
        try {
            const limit = stopIndex - startIndex + 20;
            const offset = startIndex;
            const newItems = await api.getMediaByPerson(person.id, limit, offset);

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
            console.error("Failed to load person media", error);
        } finally {
            setIsNextPageLoading(false);
        }
    };

    const handleItemClick = (item: MediaItem) => {
        setSelectedMedia(item);
        setIsViewerOpen(true);
    };

    useEffect(() => {
        setNewName(person.name || "");
    }, [person.name]);

    const handleRename = async () => {
        if (!newName.trim() || newName === person.name) {
            setIsEditing(false);
            return;
        }

        setIsSavingName(true);
        try {
            // Check for collision
            const allPeople = await api.getPeople();
            const existing = allPeople.find(p => p.name?.toLowerCase() === newName.trim().toLowerCase() && p.id !== person.id);

            if (existing) {
                setMergeTarget(existing);
                setIsMergeDialogOpen(true);
                setIsSavingName(false);
                return;
            }

            // No collision, just update
            await api.updatePersonName(person.id, newName.trim());
            toast.success("Person renamed");
            setIsEditing(false);
            if (onUpdate) {
                // Determine the new name value
                const finalName = newName.trim();
                // Optimistically update parent
                onUpdate({ ...person, name: finalName });
            }
        } catch (e) {
            console.error(e);
            toast.error("Failed to rename person");
            setIsSavingName(false);
        }
    };

    const handleMerge = async () => {
        if (!mergeTarget) return;

        try {
            await api.mergePersons(mergeTarget.id, [person.id]);
            toast.success(`Merged into ${mergeTarget.name}`);
            setIsMergeDialogOpen(false);
            onBack(true); // Go back to list and refresh
        } catch (e) {
            console.error(e);
            toast.error("Failed to merge persons");
        }
    };


    return (
        <div className="h-full w-full flex flex-col">
            <div className="flex items-center gap-2 p-4 border-b">
                <Button variant="ghost" size="icon" onClick={() => onBack()} aria-label="Back to people">
                    <ArrowLeft className="h-4 w-4" />
                </Button>

                <div className="flex-1 flex items-center gap-2">
                    {isEditing ? (
                        <div className="flex items-center gap-2 w-full max-w-sm">
                            <Input
                                value={newName}
                                onChange={(e) => setNewName(e.target.value)}
                                autoFocus
                                onKeyDown={(e) => {
                                    if (e.key === "Enter") handleRename();
                                    if (e.key === "Escape") setIsEditing(false);
                                }}
                            />
                            <Button size="icon" variant="ghost" onClick={handleRename} disabled={isSavingName} aria-label="Save name">
                                <Save className="h-4 w-4 text-green-500" />
                            </Button>
                            <Button size="icon" variant="ghost" onClick={() => setIsEditing(false)} aria-label="Cancel renaming">
                                <X className="h-4 w-4 text-red-500" />
                            </Button>
                        </div>
                    ) : (
                        <div className="flex items-center gap-2 group">
                            <h1 className="text-xl font-bold">
                                {person.name || newName || `Person ${person.id}`}
                            </h1>
                            <Button
                                size="icon"
                                variant="ghost"
                                aria-label="Rename person"
                                className="h-8 w-8 opacity-0 group-hover:opacity-100 transition-opacity"
                                onClick={() => {
                                    setNewName(person.name || "");
                                    setIsEditing(true);
                                }}
                            >
                                <Edit2 className="h-3 w-3" />
                            </Button>
                        </div>
                    )}
                </div>

                <span className="text-muted-foreground text-sm whitespace-nowrap">
                    {person.face_count} photos
                </span>
            </div>

            <div className="flex-1 overflow-hidden">
                <MediaGrid
                    items={items}
                    hasNextPage={hasNextPage}
                    isNextPageLoading={isNextPageLoading}
                    loadNextPage={loadNextPage}
                    onItemClick={handleItemClick}
                    actions={actions}
                />
            </div>

            <MediaViewer
                item={selectedMedia}
                open={isViewerOpen}
                onClose={() => setIsViewerOpen(false)}
                items={items}
                onNavigate={setSelectedMedia}
            />

            <Dialog open={isMergeDialogOpen} onOpenChange={setIsMergeDialogOpen}>
                <DialogContent>
                    <DialogHeader>
                        <DialogTitle>Merge Persons?</DialogTitle>
                        <DialogDescription>
                            A person named "{mergeTarget?.name}" already exists.
                            Do you want to merge these photos into "{mergeTarget?.name}"?
                            This action cannot be undone.
                        </DialogDescription>
                    </DialogHeader>
                    <DialogFooter>
                        <Button variant="outline" onClick={() => setIsMergeDialogOpen(false)}>Cancel</Button>
                        <Button onClick={handleMerge}>Merge</Button>
                    </DialogFooter>
                </DialogContent>
            </Dialog>
        </div>
    );
}
