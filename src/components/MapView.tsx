import { useState, useEffect, useMemo } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { MapContainer, TileLayer, Marker, Popup } from "react-leaflet";
import { api } from "@/lib/api";
import { MediaItem } from "@/types";
import { Button } from "./ui/button";
import L from "leaflet";
import MarkerClusterGroup from "react-leaflet-cluster";
import "leaflet/dist/leaflet.css";
import "leaflet.markercluster/dist/MarkerCluster.css";
import "leaflet.markercluster/dist/MarkerCluster.Default.css";
import markerIcon from "leaflet/dist/images/marker-icon.png";
import markerIcon2x from "leaflet/dist/images/marker-icon-2x.png";
import markerShadow from "leaflet/dist/images/marker-shadow.png";

// Bundled rather than fetched from unpkg. The CSP has no `img-src` entry for
// that host, so the markers could never load, and reaching out to a CDN on
// every map open is the kind of thing this app exists not to do. Vite rewrites
// these imports to hashed asset URLs served from `self`.
// Leaflet resolves its default icon URLs through a private field that its own
// types do not declare, so name the field rather than casting the prototype to any.
delete (L.Icon.Default.prototype as { _getIconUrl?: unknown })._getIconUrl;
L.Icon.Default.mergeOptions({
    iconRetinaUrl: markerIcon2x,
    iconUrl: markerIcon,
    shadowUrl: markerShadow,
});

/** Config key for the opt-in below. */
const MAP_TILES_KEY = "map_tiles_enabled";
const TILE_HOST = "tile.openstreetmap.org";

interface PhotoLocation {
    item: MediaItem;
    lat: number;
    lng: number;
}

export function MapView() {
    const [locations, setLocations] = useState<PhotoLocation[]>([]);
    const [isLoading, setIsLoading] = useState(true);
    const [tilesEnabled, setTilesEnabled] = useState<boolean | null>(null);

    useEffect(() => {
        let cancelled = false;
        api.getAllConfig()
            .then((config) => {
                if (!cancelled) {
                    setTilesEnabled(config?.[MAP_TILES_KEY] === "true");
                }
            })
            .catch((e) => {
                console.error("Failed to read the map tile setting:", e);
                if (!cancelled) setTilesEnabled(false);
            });
        return () => {
            cancelled = true;
        };
    }, []);

    const enableTiles = async () => {
        try {
            await api.setConfig(MAP_TILES_KEY, "true");
            setTilesEnabled(true);
        } catch (e) {
            console.error("Failed to save the map tile setting:", e);
        }
    };

    useEffect(() => {
        const loadLocations = async () => {
            setIsLoading(true);
            try {
                // Search with has_location filter
                const items = await api.searchFts("", { has_location: true }, 500, 0);
                const withLocation = items
                    .filter((item) => item.latitude && item.longitude)
                    .map((item) => ({
                        item,
                        lat: item.latitude!,
                        lng: item.longitude!,
                    }));
                setLocations(withLocation);
            } catch (e) {
                console.error("Failed to load locations:", e);
            } finally {
                setIsLoading(false);
            }
        };
        loadLocations();
    }, []);

    // Calculate map center from locations or default to world center
    const center = useMemo((): [number, number] => {
        if (locations.length === 0) return [20, 0];
        const latSum = locations.reduce((sum, loc) => sum + loc.lat, 0);
        const lngSum = locations.reduce((sum, loc) => sum + loc.lng, 0);
        return [latSum / locations.length, lngSum / locations.length];
    }, [locations]);

    if (isLoading) {
        return (
            <div className="h-full w-full flex items-center justify-center">
                <div className="text-muted-foreground">Loading map...</div>
            </div>
        );
    }

    if (locations.length === 0) {
        return (
            <div className="h-full w-full flex flex-col items-center justify-center gap-4">
                <div className="text-4xl">🗺️</div>
                <h2 className="text-xl font-semibold">No photos with location</h2>
                <p className="text-muted-foreground text-center max-w-md">
                    Photos with GPS coordinates will appear on this map.
                    Enable location services when taking photos to add them here.
                </p>
            </div>
        );
    }

    if (tilesEnabled === null) {
        return (
            <div className="flex h-full w-full items-center justify-center">
                <div className="text-muted-foreground">Loading map...</div>
            </div>
        );
    }

    if (!tilesEnabled) {
        return (
            <div className="flex h-full w-full flex-col items-center justify-center gap-4 p-6 text-center">
                <h2 className="text-xl font-semibold">Map tiles are turned off</h2>
                <p className="text-muted-foreground max-w-prose text-sm">
                    Drawing the map downloads tiles from {TILE_HOST}, which discloses roughly where your
                    geotagged photos were taken to that server. Nothing else in this app talks to a third
                    party unprompted, so this is off until you turn it on. Your photos are never uploaded;
                    only the map area you are looking at is requested.
                </p>
                <p className="text-muted-foreground text-sm">
                    {locations.length} photos have coordinates.
                </p>
                <Button onClick={enableTiles}>Enable map tiles</Button>
            </div>
        );
    }

    return (
        <div className="h-full w-full relative">
            <MapContainer
                center={center}
                zoom={locations.length === 1 ? 12 : 4}
                className="h-full w-full"
                scrollWheelZoom={true}
            >
                <TileLayer
                    attribution='&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors'
                    url="https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png"
                />
                <MarkerClusterGroup chunkedLoading>
                    {locations.map((loc) => (
                        <Marker
                            key={loc.item.id}
                            position={[loc.lat, loc.lng]}
                        >
                            <Popup>
                                <div className="flex flex-col gap-2 min-w-[200px]">
                                    {loc.item.thumbnail_path && (
                                        <img
                                            src={convertFileSrc(loc.item.thumbnail_path)}
                                            alt=""
                                            className="w-full h-32 object-cover rounded"
                                        />
                                    )}
                                    <div className="text-xs text-muted-foreground">
                                        {loc.item.file_path.split(/[/\\]/).pop()}
                                    </div>
                                    {loc.item.date_taken && (
                                        <div className="text-xs text-muted-foreground">
                                            {new Date(loc.item.date_taken).toLocaleDateString()}
                                        </div>
                                    )}
                                </div>
                            </Popup>
                        </Marker>
                    ))}
                </MarkerClusterGroup>
            </MapContainer>

            {/* Info overlay */}
            <div className="absolute top-4 right-4 z-[1000] bg-background/90 backdrop-blur px-3 py-1.5 rounded-lg shadow text-sm">
                📍 {locations.length} photos with location
            </div>
        </div>
    );
}
