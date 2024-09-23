import { Event } from "../common/events.mts";

export type EventUI = {
    event: Event,
    // TODO: rename to *Index (for consistency)
    keptIdx: number,
    cdagIdx: number,
    cdagKeptIdx: number,
    shownIdx: number,
    filtered: boolean,
    causallyDependsOn: EventUI[],
};

export function createEventUI(event: Event): EventUI {
    return {
        event,
        keptIdx: 0,
        cdagIdx: 0,
        cdagKeptIdx: 0,
        shownIdx: 0,
        filtered: false,
        causallyDependsOn: [],
    };
}
