import type { ViewId } from "./types";

export interface TourStep {
  view: ViewId;
  /** CSS selector of the control to spotlight; `null` centres the box. */
  target: string | null;
  title: string;
  body: string;
}

/** The first-run walkthrough, as data: which view, what to point at, what to say. */
export const steps: TourStep[] = [
  {
    view: "transfers",
    target: null,
    title: "Welcome to AmpSync",
    body: "AmpSync downloads SharePoint folders file by file and checks each one, so big downloads finish in one piece. This tour takes about 30 seconds.",
  },
  {
    view: "new",
    target: "#link",
    title: "Start with a link",
    body: "Paste a SharePoint sharing link, or copy the address bar while you're looking at the folder. The first time, AmpSync asks you to sign in.",
  },
  {
    view: "new",
    target: "#picker",
    title: "Pick what you need",
    body: "Every folder is listed with its size. Untick anything you don't need and the total updates as you go.",
  },
  {
    view: "transfers",
    target: '[data-tour="progress"]',
    title: "Watch it run",
    body: "Progress, speed and time left show up here. A file only counts as done after AmpSync has checked it against SharePoint.",
  },
  {
    view: "transfers",
    target: '[data-tour="queue"]',
    title: "Queue several downloads",
    body: "Add more links and they run one after another. Close the app or lose your connection and it picks up where it stopped.",
  },
  {
    view: "signins",
    target: '[data-go="signins"]',
    title: "Sign-ins",
    body: "If your SharePoint sign-in expires, AmpSync pauses and waits here. Nothing already downloaded is lost.",
  },
  {
    view: "settings",
    target: "#replay",
    title: "Make it yours",
    body: "Change the download folder, how many files run at once, and a speed limit. You can replay this tour from here any time.",
  },
];
