import { Layout } from "./components/Layout";
import { Gallery } from "./components/Gallery";
import { Albums } from "./components/Albums";
import { Search } from "./components/Search";

import { Favorites } from "./components/Favorites";
import { Trash } from "./components/Trash";
import { Archive } from "./components/Archive";
import { UploadQueue } from "./components/UploadQueue";
// Lazily imported. Leaflet and its cluster plugin are the largest thing in the
// bundle by a distance, and Settings is the largest single component; most
// sessions open neither.
const MapView = lazy(() => import("./components/MapView").then((m) => ({ default: m.MapView })));
const Settings = lazy(() => import("./components/Settings").then((m) => ({ default: m.Settings })));
import { DuplicateReview } from "./components/DuplicateReview";
import { People } from "./components/People";
import { Tags } from "./components/Tags";
import { SmartAlbums } from "./components/SmartAlbums";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { Onboarding } from "./components/Onboarding";
import "./App.css";

/** Shown while a lazily imported view's chunk is fetched. */
function ViewLoading() {
  return (
    <div className="text-muted-foreground flex h-full items-center justify-center">Loading...</div>
  );
}

/** Human names for the views, used when a per-view boundary reports a crash. */
const VIEW_LABELS: Record<string, string> = {
  timeline: "The timeline",
  albums: "Albums",
  favorites: "Favorites",
  trash: "Trash",
  archive: "The archive",
  uploads: "The upload queue",
  map: "The map",
  duplicates: "Duplicate review",
  people: "People",
  tags: "Tags",
  "smart-albums": "Smart albums",
  search: "Search",
  settings: "Settings",
};

import { lazy, Suspense, useEffect, useState } from "react";
import { api, hasErrorCode } from "./lib/api";

import { Toaster } from "@/components/ui/sonner";

function App() {
  const [view, setView] = useState("timeline");
  const [securityStatus, setSecurityStatus] = useState<{
    onboardingComplete: boolean;
    securityMode: string;
    encryptionConfigured: boolean;
    encryptionLocked: boolean;
    telegramCredentialsConfigured: boolean;
    migration: {
      running: boolean;
      total: number;
      processed: number;
      succeeded: number;
      failed: number;
      lastError?: string | null;
    };
  } | null>(null);
  const [securityLoading, setSecurityLoading] = useState(true);

  const refreshSecurityStatus = async () => {
    try {
      const status = await api.getSecurityStatus();
      setSecurityStatus(status);
      setSecurityLoading(false);
    } catch (e) {
      // Startup opens the database after the window exists, so the first call can
      // legitimately arrive too early. This used to match on the message text, which
      // meant rewording an error string silently turned the retry into a dead splash
      // screen.
      if (hasErrorCode(e, "databaseNotInitialized")) {
        setTimeout(() => {
          refreshSecurityStatus();
        }, 250);
        return;
      }
      console.error("Failed to load security status", e);
      setSecurityLoading(false);
    }
  };

  useEffect(() => {
    refreshSecurityStatus();
  }, []);

  useEffect(() => {
    if (!securityStatus) return;
    if (securityStatus.securityMode !== "encrypted") return;
    if (securityStatus.encryptionLocked) return;

    api.startEncryptionMigration().catch((e) => {
      console.warn("Auto-resume migration skipped:", e);
    });
  }, [securityStatus]);

  const requiresGate =
    securityStatus &&
    (!securityStatus.onboardingComplete ||
      (securityStatus.securityMode === "encrypted" && securityStatus.encryptionLocked));

  return (
    <>
      {securityLoading ? (
        <div className="h-screen w-screen flex items-center justify-center">
          <p className="text-muted-foreground">Loading secure startup...</p>
        </div>
      ) : requiresGate && securityStatus ? (
        <Onboarding status={securityStatus} onReady={refreshSecurityStatus} />
      ) : (
        <Layout currentView={view} onViewChange={setView}>
          {/*
            Keyed by view so that switching views resets a boundary that has
            already caught something: without the key, one crash would leave
            the fallback in place for every view the user navigated to next.
            Per-view rather than one boundary around Layout, so that a broken
            view leaves the sidebar and the rest of the app usable.
          */}
          <ErrorBoundary key={view} context={VIEW_LABELS[view]} inline>
            <div className="h-full overflow-hidden">
              {view === 'timeline' && <Gallery />}
              {view === 'albums' && <Albums />}
              {view === 'favorites' && <Favorites />}
              {view === 'trash' && <Trash />}
              {view === 'archive' && <Archive />}
              {view === 'uploads' && <UploadQueue />}
              {view === 'map' && (
                <Suspense fallback={<ViewLoading />}>
                  <MapView />
                </Suspense>
              )}
              {view === 'duplicates' && <DuplicateReview />}
              {view === 'people' && <People />}
              {view === 'tags' && <Tags />}
              {view === 'smart-albums' && <SmartAlbums />}
              {view === 'search' && <Search />}
              {view === 'settings' && (
                <Suspense fallback={<ViewLoading />}>
                  <Settings />
                </Suspense>
              )}
            </div>
          </ErrorBoundary>
        </Layout>
      )}
      <Toaster />
    </>
  );
}

export default App;

