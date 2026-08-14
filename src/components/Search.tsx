import { useState, useCallback, useEffect } from "react";
import { useLatestRequest } from "@/hooks/use-latest-request";
import { MediaItem, SearchFilters, Tag } from "../types";
import { api } from "../lib/api";
import { MediaGrid } from "./MediaGrid";
import { Input } from "./ui/input";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";
import { Label } from "./ui/label";
import {
    Select,
    SelectContent,
    SelectItem,
    SelectTrigger,

    SelectValue,
} from "./ui/select";
import { Badge } from "./ui/badge";
import { Search as SearchIcon, Heart, Star, Filter, X, Clock, Camera, MapPin, Sparkles } from "lucide-react";

const SEARCH_HISTORY_KEY = "wanderer_search_history";
const MAX_HISTORY_ITEMS = 10;

function getSearchHistory(): string[] {
    try {
        const stored = localStorage.getItem(SEARCH_HISTORY_KEY);
        if (!stored) return [];
        const parsed = JSON.parse(stored);
        if (!Array.isArray(parsed)) return [];
        return parsed.filter((entry): entry is string => typeof entry === "string");
    } catch {
        return [];
    }
}

function saveSearchHistory(history: string[]) {
    localStorage.setItem(SEARCH_HISTORY_KEY, JSON.stringify(history.slice(0, MAX_HISTORY_ITEMS)));
}

export function Search() {
    const [items, setItems] = useState<MediaItem[]>([]);
    const [query, setQuery] = useState("");
    const [hasNextPage, setHasNextPage] = useState(true);
    const [isNextPageLoading, setIsNextPageLoading] = useState(false);
    const [hasSearched, setHasSearched] = useState(false);
    const beginSearch = useLatestRequest();

    // Filter states
    const [showFilters, setShowFilters] = useState(false);
    const [favoritesOnly, setFavoritesOnly] = useState(false);
    const [minRating, setMinRating] = useState<string>("0");
    const [cameraMake, setCameraMake] = useState("");
    const [hasLocation, setHasLocation] = useState<string>("any");

    // Tag state
    const [tags, setTags] = useState<Tag[]>([]);
    const [selectedTag, setSelectedTag] = useState<string | null>(null);

    // Search history
    const [searchHistory, setSearchHistory] = useState<string[]>([]);
    const [showHistory, setShowHistory] = useState(false);
    // AI Search
    const [isAiSearch, setIsAiSearch] = useState(false);

    const loadTags = useCallback(async () => {
        try {
            const data = await api.getAllTags();
            setTags(data);
        } catch (e) {
            console.error("Failed to load tags:", e);
        }
    }, []);

    useEffect(() => {
        setSearchHistory(getSearchHistory());
        loadTags();

        // Keep tags reasonably fresh while AI worker is processing.
        const interval = setInterval(loadTags, 10000);
        return () => clearInterval(interval);
    }, [loadTags]);

    // Trigger search when tag changes
    useEffect(() => {
        // Only trigger if we have interacted (skip initial mount if null)
        // But initial mount is null, so it might query empty?
        // Actually performSearch handles query "" by fetching recent/all?
        // If selectedTag is set, performSearch will use it.
        // We trigger search whenever selectedTag changes.
        if (selectedTag !== null) {
            performSearch("", 0, 20, true);
        } else if (hasSearched) {
            // If we deselected tag, re-run search with current query (or empty)
            performSearch(query, 0, 20, true);
        }
    }, [selectedTag]);

    const addToHistory = (searchQuery: string) => {
        if (!searchQuery.trim()) return;
        const newHistory = [searchQuery, ...searchHistory.filter(h => h !== searchQuery)].slice(0, MAX_HISTORY_ITEMS);
        setSearchHistory(newHistory);
        saveSearchHistory(newHistory);
    };

    const removeFromHistory = (searchQuery: string) => {
        const newHistory = searchHistory.filter(h => h !== searchQuery);
        setSearchHistory(newHistory);
        saveSearchHistory(newHistory);
    };

    const clearHistory = () => {
        setSearchHistory([]);
        localStorage.removeItem(SEARCH_HISTORY_KEY);
    };

    const createFilters = useCallback((): SearchFilters => {
        return {
            favorites_only: favoritesOnly,
            min_rating: parseInt(minRating) > 0 ? parseInt(minRating) : undefined,
            camera_make: cameraMake.trim() || undefined,
            has_location: hasLocation === "any" ? undefined : hasLocation === "yes",
        };
    }, [favoritesOnly, minRating, cameraMake, hasLocation]);

    const performSearch = async (
        searchQuery: string,
        startIndex: number,
        stopIndex: number,
        isNewSearch: boolean
    ) => {
        // Typing produces overlapping searches, and semantic search in particular
        // takes long enough that an earlier query routinely lands after a later
        // one. Everything below writes state only while this is still the newest.
        const isCurrent = beginSearch();
        setIsNextPageLoading(true);
        try {
            const limit = stopIndex - startIndex + 20;
            const offset = startIndex;
            let newItems: MediaItem[] = [];

            if (selectedTag) {
                // Tag Search (Higher priority than AI or FTS if selected)
                newItems = await api.getMediaByTag(selectedTag, limit, offset);
            } else if (isAiSearch) {
                // Semantic Search
                // Note: Semantic search doesn't support offset pagination in the same way (it ranks everything)
                // So we always fetch top-K. For simplicity, we just fetch a reasonable limit.
                // If we want pagination, we'd need to cache the results or support it in backend.
                // For now, if offset > 0, we might just stop or fetch more.
                // Let's assume unlimited scroll isn't perfectly supported for AI search yet or we just fetch top 100.
                if (startIndex === 0) {
                    newItems = await api.semanticSearch(searchQuery, 100);
                } else {
                    // Stop pagination for now for AI search
                    newItems = [];
                }
            } else {
                // Standard FTS Search
                const filters = createFilters();
                newItems = await api.searchFts(searchQuery, filters, limit, offset);
            }

            if (!isCurrent()) return;

            if (newItems.length === 0) {
                setHasNextPage(false);
            }

            if (isNewSearch) {
                setItems(newItems);
                setHasNextPage(newItems.length > 0);
                if (searchQuery.trim() && !selectedTag) {
                    addToHistory(searchQuery);
                }
            } else {
                setItems(prev => {
                    const existingIds = new Set(prev.map(i => i.id));
                    const filtered = newItems.filter(i => !existingIds.has(i.id));
                    return [...prev, ...filtered];
                });
            }
            setHasSearched(true);
        } catch (error) {
            console.error("Failed to search media", error);
        } finally {
            if (isCurrent()) {
                setIsNextPageLoading(false);
            }
        }
    };

    const loadNextPage = async (startIndex: number, stopIndex: number) => {
        if (isNextPageLoading) return;
        await performSearch(query, startIndex, stopIndex, false);
    };

    const handleSearch = (e: React.FormEvent) => {
        e.preventDefault();
        setShowHistory(false);
        performSearch(query, 0, 20, true);
    };

    const handleHistorySelect = (historyQuery: string) => {
        setQuery(historyQuery);
        setShowHistory(false);
        performSearch(historyQuery, 0, 20, true);
    };

    const clearFilters = () => {
        setFavoritesOnly(false);
        setMinRating("0");
        setCameraMake("");
        setHasLocation("any");
    };


    const handleTagSelect = (tagName: string) => {
        if (selectedTag === tagName) {
            setSelectedTag(null);
        } else {
            // Clear other search modes when selecting a tag
            setQuery("");
            setIsAiSearch(false);
            setFavoritesOnly(false);
            setMinRating("0");
            setCameraMake("");
            setHasLocation("any");
            setSelectedTag(tagName);
        }
    };

    const hasActiveFilters = favoritesOnly || parseInt(minRating) > 0 || cameraMake.trim() !== "" || hasLocation !== "any";

    return (
        <div className="h-full w-full flex flex-col">
            {/* Search Header */}
            <div className="p-4 border-b space-y-3">
                <form onSubmit={handleSearch} className="flex gap-2 max-w-2xl mx-auto">
                    <div className="relative flex-1">
                        <SearchIcon className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                        <Input
                            placeholder={isAiSearch ? "Describe what you're looking for..." : "Search files, tags, people..."}
                            className={isAiSearch ? "pl-9 border-purple-500 ring-purple-500 focus-visible:ring-purple-500" : "pl-9"}
                            value={query}
                            onChange={(e) => setQuery(e.target.value)}
                            onFocus={() => searchHistory.length > 0 && setShowHistory(true)}
                            onBlur={() => setTimeout(() => setShowHistory(false), 200)}
                        />

                        {/* Search History Dropdown */}
                        {showHistory && searchHistory.length > 0 && (
                            <div className="absolute top-full left-0 right-0 mt-1 bg-popover border rounded-lg shadow-lg z-50 overflow-hidden">
                                <div className="flex items-center justify-between px-3 py-2 border-b bg-muted/50">
                                    <span className="text-xs font-medium text-muted-foreground">Recent searches</span>
                                    <Button
                                        type="button"
                                        variant="ghost"
                                        size="sm"
                                        onClick={clearHistory}
                                        className="h-6 text-xs"
                                    >
                                        Clear all
                                    </Button>
                                </div>
                                <div className="max-h-48 overflow-y-auto">
                                    {searchHistory.map((historyItem) => (
                                        <div
                                            key={historyItem}
                                            className="flex items-center gap-2 px-3 py-2 hover:bg-muted cursor-pointer group"
                                            onClick={() => handleHistorySelect(historyItem)}
                                        >
                                            <Clock className="h-3.5 w-3.5 text-muted-foreground" />
                                            <span className="flex-1 text-sm truncate">{historyItem}</span>
                                            <Button
                                                type="button"
                                                variant="ghost"
                                                size="icon"
                                                className="h-6 w-6 opacity-0 group-hover:opacity-100"
                                                onClick={(e) => {
                                                    e.stopPropagation();
                                                    removeFromHistory(historyItem);
                                                }}
                                            >
                                                <X className="h-3 w-3" />
                                            </Button>
                                        </div>
                                    ))}
                                </div>
                            </div>
                        )}
                    </div>
                    <div className="flex items-center gap-2">
                        <Button
                            type="button"
                            variant={isAiSearch ? "default" : "outline"}
                            size="icon"
                            onClick={() => setIsAiSearch(!isAiSearch)}
                            title="Toggle AI Semantic Search"
                            className={isAiSearch ? "bg-purple-600 hover:bg-purple-700 text-white border-purple-600" : ""}
                        >
                            <Sparkles className="h-4 w-4" />
                        </Button>
                        <Button
                            type="button"
                            variant={showFilters ? "secondary" : "outline"}
                            size="icon"
                            onClick={() => setShowFilters(!showFilters)}
                            className="relative"
                            disabled={isAiSearch} // Disable filters in AI mode for now
                        >
                            <Filter className="h-4 w-4" />
                            {hasActiveFilters && (
                                <span className="absolute -top-1 -right-1 w-2 h-2 bg-primary rounded-full" />
                            )}
                        </Button>
                    </div>
                    <Button type="submit">Search</Button>
                </form>

                {/* Filter Panel */}
                {showFilters && (
                    <div className="max-w-2xl mx-auto flex flex-wrap items-center gap-4 p-3 bg-muted/50 rounded-lg">
                        {/* Favorites Toggle */}
                        <div className="flex items-center gap-2">
                            <Switch
                                id="favorites-filter"
                                checked={favoritesOnly}
                                onCheckedChange={setFavoritesOnly}
                            />
                            <Label htmlFor="favorites-filter" className="flex items-center gap-1.5 cursor-pointer">
                                <Heart className="h-4 w-4 text-red-500" />
                                Favorites only
                            </Label>
                        </div>

                        {/* Rating Filter */}
                        <div className="flex items-center gap-2">
                            <Star className="h-4 w-4 text-yellow-500" />
                            <Select value={minRating} onValueChange={setMinRating}>
                                <SelectTrigger className="w-[140px]">
                                    <SelectValue placeholder="Min rating" />
                                </SelectTrigger>
                                <SelectContent>
                                    <SelectItem value="0">Any rating</SelectItem>
                                    <SelectItem value="1">⭐ 1+ stars</SelectItem>
                                    <SelectItem value="2">⭐⭐ 2+ stars</SelectItem>
                                    <SelectItem value="3">⭐⭐⭐ 3+ stars</SelectItem>
                                    <SelectItem value="4">⭐⭐⭐⭐ 4+ stars</SelectItem>
                                    <SelectItem value="5">⭐⭐⭐⭐⭐ 5 stars</SelectItem>
                                </SelectContent>
                            </Select>
                        </div>

                        {/* Camera Filter */}
                        <div className="flex items-center gap-2">
                            <Camera className="h-4 w-4 text-muted-foreground" />
                            <Input
                                placeholder="Camera make"
                                value={cameraMake}
                                onChange={(e) => setCameraMake(e.target.value)}
                                className="w-[140px]"
                            />
                        </div>

                        {/* Location Filter */}
                        <div className="flex items-center gap-2">
                            <MapPin className="h-4 w-4 text-green-600" />
                            <Select value={hasLocation} onValueChange={setHasLocation}>
                                <SelectTrigger className="w-[140px]">
                                    <SelectValue placeholder="Location" />
                                </SelectTrigger>
                                <SelectContent>
                                    <SelectItem value="any">Any</SelectItem>
                                    <SelectItem value="yes">Has location</SelectItem>
                                    <SelectItem value="no">No location</SelectItem>
                                </SelectContent>
                            </Select>
                        </div>

                        {/* Clear Filters */}
                        {hasActiveFilters && (
                            <Button
                                variant="ghost"
                                size="sm"
                                onClick={clearFilters}
                                className="ml-auto"
                            >
                                <X className="h-3 w-3 mr-1" />
                                Clear filters
                            </Button>
                        )}
                    </div>
                )}
                {/* Tag Filters */}
                {tags.length > 0 && !isAiSearch && (
                    <div className="px-4 pb-2 flex gap-2 overflow-x-auto no-scrollbar mask-fade-right">
                        {tags.map(tag => (
                            <Badge
                                key={tag.id}
                                variant={selectedTag === tag.name ? "default" : "outline"}
                                className="cursor-pointer whitespace-nowrap hover:bg-secondary/80 transition-colors"
                                onClick={() => handleTagSelect(tag.name)}
                            >
                                {tag.name} <span className="ml-1 opacity-60 text-[10px]">{tag.media_count}</span>
                            </Badge>
                        ))}
                    </div>
                )}
            </div>

            {/* Results */}
            <div className="flex-1 overflow-hidden">
                {items.length === 0 && hasSearched ? (
                    <div className="flex flex-col items-center justify-center h-full text-muted-foreground">
                        <SearchIcon className="w-12 h-12 mb-3 text-muted-foreground/30" />
                        <p>No results found{query && ` for "${query}"`}</p>
                        {hasActiveFilters && (
                            <p className="text-sm mt-1">Try adjusting your filters</p>
                        )}
                    </div>
                ) : items.length === 0 ? (
                    <div className="flex flex-col items-center justify-center h-full text-muted-foreground">
                        <SearchIcon className="w-12 h-12 mb-3 text-muted-foreground/30" />
                        <p className="font-medium">Search your media</p>
                        <p className="text-sm mt-1">Enter a search term or use filters to find media</p>
                    </div>
                ) : (
                    <MediaGrid
                        items={items}
                        hasNextPage={hasNextPage}
                        isNextPageLoading={isNextPageLoading}
                        loadNextPage={loadNextPage}
                    />
                )}
            </div>
        </div>
    );
}
