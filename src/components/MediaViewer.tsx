import { useRef, useState, useEffect, useCallback } from "react";
import { useLatestRequest } from "@/hooks/use-latest-request";
import { Dialog, DialogContent } from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { MediaItem, Face } from "@/types";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/lib/api";

interface MediaViewerProps {
    item: MediaItem | null;
    open: boolean;
    onClose: () => void;
}

interface TouchPoint {
    x: number;
    y: number;
}

export function MediaViewer({ item, open, onClose }: MediaViewerProps) {
    const [faces, setFaces] = useState<Face[]>([]);
    const [tags, setTags] = useState<string[]>([]);
    const [imgState, setImgState] = useState<{ clientW: number; clientH: number; naturalW: number; naturalH: number } | null>(null);
    const [viewPath, setViewPath] = useState<string>("");
    const [isLoadingCloud, setIsLoadingCloud] = useState(false);
    const beginItemLoad = useLatestRequest();

    // Zoom and pan state
    const [scale, setScale] = useState(1);
    const [translate, setTranslate] = useState<TouchPoint>({ x: 0, y: 0 });
    const [isPanning, setIsPanning] = useState(false);

    // Touch tracking refs
    const lastTouchDistance = useRef<number | null>(null);
    const lastTouchCenter = useRef<TouchPoint | null>(null);
    const lastTapTime = useRef<number>(0);
    const lastPanPoint = useRef<TouchPoint | null>(null);
    const translateRef = useRef<TouchPoint>({ x: 0, y: 0 });
    const pendingTranslateRef = useRef<TouchPoint | null>(null);
    const panRafRef = useRef<number | null>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const imgRef = useRef<HTMLImageElement>(null);

    const setTranslateImmediate = useCallback((next: TouchPoint) => {
        if (panRafRef.current !== null) {
            cancelAnimationFrame(panRafRef.current);
            panRafRef.current = null;
        }
        pendingTranslateRef.current = null;
        translateRef.current = next;
        setTranslate(next);
    }, []);

    const queueTranslate = useCallback((next: TouchPoint) => {
        translateRef.current = next;
        pendingTranslateRef.current = next;
        if (panRafRef.current !== null) return;
        panRafRef.current = requestAnimationFrame(() => {
            panRafRef.current = null;
            if (!pendingTranslateRef.current) return;
            setTranslate(pendingTranslateRef.current);
            pendingTranslateRef.current = null;
        });
    }, []);

    useEffect(() => {
        return () => {
            if (panRafRef.current !== null) {
                cancelAnimationFrame(panRafRef.current);
            }
        };
    }, []);

    // Reset zoom when item changes or viewer closes
    useEffect(() => {
        // Paging through an encrypted library decrypts and materialises each file,
        // which takes long enough that a previous item routinely resolves after the
        // current one. Without this guard the viewer shows the wrong photo, and in
        // encrypted mode it is a photo the user did not ask to decrypt.
        const isCurrent = beginItemLoad();

        if (item && open) {
            // Reset zoom/pan states
            setScale(1);
            setTranslateImmediate({ x: 0, y: 0 });
            setFaces([]);
            setTags([]);
            setImgState(null);

            if (item.is_cloud_only) {
                setIsLoadingCloud(true);
                setViewPath("");
                api.downloadForView(item.id)
                    .then(path => {
                        if (!isCurrent()) return;
                        setViewPath(path);
                    })
                    .catch(err => {
                        console.error("Failed to download cloud file:", err);
                    })
                    .finally(() => {
                        if (isCurrent()) setIsLoadingCloud(false);
                    });
            } else {
                setViewPath(item.file_path);
                setIsLoadingCloud(false);
            }

            // Load faces
            api.getFaces(item.id)
                .then(faces => {
                    if (!isCurrent()) return;
                    setFaces(faces);
                })
                .catch(err => console.error("Failed to load faces:", err));

            // Load tags
            api.getTagsForMedia(item.id)
                .then(loaded => {
                    if (!isCurrent()) return;
                    setTags(loaded);
                })
                .catch(console.error);
        } else {
            setFaces([]);
            setTags([]);
            setImgState(null);
            setViewPath("");
            setScale(1);
            setTranslateImmediate({ x: 0, y: 0 });
            setIsPanning(false);
            lastPanPoint.current = null;
        }
    }, [item, open, setTranslateImmediate, beginItemLoad]);

    const handleImageLoad = (e: React.SyntheticEvent<HTMLImageElement>) => {
        const { clientWidth, clientHeight, naturalWidth, naturalHeight } = e.currentTarget;
        setImgState({
            clientW: clientWidth,
            clientH: clientHeight,
            naturalW: naturalWidth,
            naturalH: naturalHeight
        });
    };

    // Calculate distance between two touch points
    const getTouchDistance = (touches: React.TouchList): number => {
        if (touches.length < 2) return 0;
        const dx = touches[0].clientX - touches[1].clientX;
        const dy = touches[0].clientY - touches[1].clientY;
        return Math.sqrt(dx * dx + dy * dy);
    };

    // Calculate center point between two touches
    const getTouchCenter = (touches: React.TouchList): TouchPoint => {
        if (touches.length < 2) {
            return { x: touches[0].clientX, y: touches[0].clientY };
        }
        return {
            x: (touches[0].clientX + touches[1].clientX) / 2,
            y: (touches[0].clientY + touches[1].clientY) / 2
        };
    };

    // Handle touch start
    const handleTouchStart = useCallback((e: React.TouchEvent) => {
        if (e.touches.length === 2) {
            // Pinch gesture start
            lastTouchDistance.current = getTouchDistance(e.touches);
            lastTouchCenter.current = getTouchCenter(e.touches);
        } else if (e.touches.length === 1) {
            // Single touch - check for double tap or pan
            const now = Date.now();
            const timeSinceLastTap = now - lastTapTime.current;

            if (timeSinceLastTap < 300) {
                // Double tap detected - reset zoom
                setScale(1);
                setTranslateImmediate({ x: 0, y: 0 });
                lastTapTime.current = 0;
            } else {
                lastTapTime.current = now;
                if (scale > 1) {
                    // Start panning if zoomed in
                    setIsPanning(true);
                    lastTouchCenter.current = { x: e.touches[0].clientX, y: e.touches[0].clientY };
                }
            }
        }
    }, [scale, setTranslateImmediate]);

    // Handle touch move
    const handleTouchMove = useCallback((e: React.TouchEvent) => {
        if (e.touches.length === 2 && lastTouchDistance.current !== null) {
            // Pinch zoom
            e.preventDefault();
            const newDistance = getTouchDistance(e.touches);
            const scaleFactor = newDistance / lastTouchDistance.current;

            setScale(prev => {
                const newScale = prev * scaleFactor;
                return Math.min(Math.max(newScale, 0.5), 5); // Clamp between 0.5x and 5x
            });

            lastTouchDistance.current = newDistance;
        } else if (e.touches.length === 1 && isPanning && lastTouchCenter.current && scale > 1) {
            // Pan while zoomed
            e.preventDefault();
            const dx = e.touches[0].clientX - lastTouchCenter.current.x;
            const dy = e.touches[0].clientY - lastTouchCenter.current.y;

            queueTranslate({
                x: translateRef.current.x + dx,
                y: translateRef.current.y + dy,
            });

            lastTouchCenter.current = { x: e.touches[0].clientX, y: e.touches[0].clientY };
        }
    }, [isPanning, queueTranslate, scale]);

    // Handle touch end
    const handleTouchEnd = useCallback(() => {
        lastTouchDistance.current = null;
        lastTouchCenter.current = null;
        setIsPanning(false);
        lastPanPoint.current = null;

        // Snap back to 1x if close to it
        setScale(prev => {
            if (prev > 0.9 && prev < 1.1) return 1;
            return prev;
        });
    }, []);

    // Handle wheel zoom for desktop
    const handleWheel = useCallback((e: React.WheelEvent) => {
        e.preventDefault();
        const delta = e.deltaY > 0 ? 0.9 : 1.1;
        setScale(prev => Math.min(Math.max(prev * delta, 0.5), 5));
    }, []);

    const handleDoubleClick = useCallback((e: React.MouseEvent) => {
        if (item?.mime_type?.startsWith("video/")) return;
        e.preventDefault();
        if (scale > 1) {
            setScale(1);
            setTranslateImmediate({ x: 0, y: 0 });
        } else {
            setScale(2);
        }
    }, [item?.mime_type, scale, setTranslateImmediate]);

    const handleMouseDown = useCallback((e: React.MouseEvent) => {
        if (item?.mime_type?.startsWith("video/")) return;
        if (e.button !== 0 || scale <= 1) return;
        e.preventDefault();
        setIsPanning(true);
        lastPanPoint.current = { x: e.clientX, y: e.clientY };
    }, [item?.mime_type, scale]);

    const handleMouseMove = useCallback((e: React.MouseEvent) => {
        if (!isPanning || scale <= 1 || !lastPanPoint.current) return;
        e.preventDefault();
        const dx = e.clientX - lastPanPoint.current.x;
        const dy = e.clientY - lastPanPoint.current.y;
        queueTranslate({
            x: translateRef.current.x + dx,
            y: translateRef.current.y + dy,
        });
        lastPanPoint.current = { x: e.clientX, y: e.clientY };
    }, [isPanning, queueTranslate, scale]);

    const stopMousePan = useCallback(() => {
        setIsPanning(false);
        lastPanPoint.current = null;
    }, []);

    if (!item) return null;

    return (
        <Dialog open={open} onOpenChange={(val) => !val && onClose()}>
            <DialogContent className="max-w-[95vw] max-h-[90vh] p-0 overflow-hidden bg-black/90 border-none">
                <div
                    ref={containerRef}
                    className={`relative w-full h-full flex items-center justify-center p-4 touch-none ${scale > 1 ? (isPanning ? "cursor-grabbing" : "cursor-grab") : ""}`}
                    onTouchStart={handleTouchStart}
                    onTouchMove={handleTouchMove}
                    onTouchEnd={handleTouchEnd}
                    onWheel={handleWheel}
                    onDoubleClick={handleDoubleClick}
                    onMouseDown={handleMouseDown}
                    onMouseMove={handleMouseMove}
                    onMouseUp={stopMousePan}
                    onMouseLeave={stopMousePan}
                >
                    <div
                        className={`relative inline-block will-change-transform ${isPanning ? "transition-none" : "transition-transform duration-100 ease-out"}`}
                        style={{
                            transform: `translate3d(${translate.x}px, ${translate.y}px, 0) scale(${scale})`,
                            transformOrigin: 'center center'
                        }}
                    >
                        {isLoadingCloud ? (
                            <div className="flex flex-col items-center justify-center text-white gap-4 p-8">
                                <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-white"></div>
                                <span className="text-sm">Downloading from Cloud...</span>
                            </div>
                        ) : (
                            viewPath && (
                                item.mime_type?.startsWith('video/') ? (
                                    <video
                                        src={convertFileSrc(viewPath)}
                                        controls
                                        autoPlay
                                        className="max-h-[85vh] max-w-full object-contain rounded-md"
                                    />
                                ) : (
                                    <img
                                        ref={imgRef}
                                        src={convertFileSrc(viewPath)}
                                        alt={`Media ${item.id}`}
                                        className="max-h-[85vh] max-w-full object-contain rounded-md select-none"
                                        onLoad={handleImageLoad}
                                        draggable={false}
                                    />
                                )
                            )
                        )}

                        {/* Face Overlays */}
                        {imgState && faces.map((face, index) => {
                            const scaleX = imgState.clientW / imgState.naturalW;
                            const scaleY = imgState.clientH / imgState.naturalH;

                            return (
                                <div
                                    key={index}
                                    className="absolute border-4 border-red-500 bg-transparent hover:bg-red-500/20 transition-colors z-50 cursor-pointer"
                                    style={{
                                        left: face.x * scaleX,
                                        top: face.y * scaleY,
                                        width: face.width * scaleX,
                                        height: face.height * scaleY,
                                    }}
                                    title={`Face Score: ${(face.score * 100).toFixed(1)}%`}
                                />
                            );
                        })}
                    </div>

                    {/* Tags Overlay */}
                    {tags.length > 0 && (
                        <div className="absolute top-4 left-4 flex gap-2 flex-wrap max-w-[80%] z-50 pointer-events-none">
                            {tags.map((tag, i) => (
                                <Badge key={i} variant="secondary" className="bg-black/50 text-white border-none backdrop-blur-sm pointer-events-auto">
                                    {tag}
                                </Badge>
                            ))}
                        </div>
                    )}

                    {/* Zoom indicator */}
                    {scale !== 1 && (
                        <div className="absolute bottom-4 left-1/2 -translate-x-1/2 bg-black/70 text-white px-3 py-1 rounded-full text-sm">
                            {Math.round(scale * 100)}%
                        </div>
                    )}
                </div>
            </DialogContent>
        </Dialog>
    );
}

