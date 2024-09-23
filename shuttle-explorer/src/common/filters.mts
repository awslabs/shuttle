/**
 * Filters are a system to hide events from the timeline based on various
 * criteria, such as the kind of event, which object it is related to, whether
 * it happens-before another event, etc.
 */

import {
    Event,
    EventCategory,
    eventCategories,
} from "./events.mts";

import {
    ExtContextData,
} from "./context.mts";
import { clockCmp, Ordering } from "./clocks.mts";

/**
 * A filter for events.
 */
export interface Filter<TData> {
    /**
     * Title to show in the filters list. (Not shown for built-in filters.)
     */
    title: string,

    /**
     * Is the filter currently enabled?
     */
    enabled: boolean,

    /**
     * Is the filter built-in and thus always present in the filter chain?
     */
    builtin: boolean,

    /**
     * Any state that the filter makes use of.
     */
    data: TData,

    /**
     * How many events *in total*, i.e., without applying prior filters in the
     * chain, pass through this filter? Set after initialisation.
     */
    totalCount?: number,

    /**
     * Initialises the filter's state.
     */
    init?: (self: TData, events: Event[]) => void,

    /**
     * Checks whether the given event passes through this filter.
     * @returns `true` iff the event passes, i.e., it should be shown in the
     * timeline (assuming other filters don't filter it out).
     */
    check: (self: TData, event: Event) => boolean,
};

/**
 * A helper type to declare filter types which satisfy the `Filter` interface.
 */
type BaseFilter<TData> = {
    title: string,
    enabled: boolean,
    builtin: boolean,
    data: TData,
    totalCount?: number,
    init?: (self: TData, events: Event[]) => void,
    check: (self: TData, event: Event) => boolean,
};

type FilterByCategoryData = {
    /**
     * Which categories are currently enabled?
     */
    enabledCategories: {[key in EventCategory]?: boolean},

    /**
     * How many events in each category?
     */
    categoryCounts: {[key in EventCategory]?: number},
};
/**
 * Categories enabled by default.
 */
const categoriesEnabledByDefault: EventCategory[] = [
    "semaphore",
    "task",
    "random",
];
export const filterByCategory: BaseFilter<FilterByCategoryData> = {
    title: "Category filter",
    enabled: true,
    builtin: true,
    data: {
        enabledCategories: {},
        categoryCounts: {},
    },
    init: (self, events) => {
        for (const category of eventCategories) {
            self.enabledCategories[category] = categoriesEnabledByDefault.includes(category);
            self.categoryCounts[category] = 0;
        }

        // count number of events per category
        for (const event of events) {
            self.categoryCounts[event.category!]!++;
        }
    },
    check: (self, {category}) => self.enabledCategories[category!]!,
};

/**
 * Initialises a filter with stats, such as the total number of events that the
 * filter keeps.
 * @param filter Filter to initialise.
 * @returns Initialised filter.
 */
export function initFilter(ctx: ExtContextData, filter: Filter<any>): Filter<any> {
    if (filter.init) {
        filter.init(filter.data, ctx.events);
    }
    filter.totalCount = ctx.events.filter(event => filter.check(filter.data, event)).length;
    return filter;
}

/**
 * Creates a filter that only keeps or hides events related to a specific object.
 * @param objectId Object ID to keep or hid.
 * @param keepObject If `true`, related events are kept, otherwise they are filtered.
 * @returns A filter which keeps events related to the given object.
 */
export function filterByObject(ctx: ExtContextData, objectId: number, keepObject: boolean): Filter<{}> {
    return initFilter(ctx, {
        title: `Show only events ${keepObject ? "with" : "without"} object ${objectId}`,
        enabled: true,
        builtin: false,
        data: {},
        check: (_self, {category, data}) => (category === "semaphore" && data.objectId === objectId) == keepObject,
    });
}

export function filterByEventBefore(ctx: ExtContextData, eventId: number): Filter<{}> {
    const refEvent = ctx.events[eventId];
    return initFilter(ctx, {
        title: `Show only events causally before event ${eventId}`,
        enabled: true,
        builtin: false,
        data: {},
        check: (_self, event) => event.id === refEvent.id || clockCmp(event, refEvent) === Ordering.Less,
    });
}

export function filterByEventAfter(ctx: ExtContextData, eventId: number): Filter<{}> {
    const refEvent = ctx.events[eventId];
    return initFilter(ctx, {
        title: `Show only events causally after event ${eventId}`,
        enabled: true,
        builtin: false,
        data: {},
        check: (_self, event) => event.id === refEvent.id || clockCmp(refEvent, event) === Ordering.Less,
    });
}

export function filterByEventConcurrent(ctx: ExtContextData, eventId: number): Filter<{}> {
    const refEvent = ctx.events[eventId];
    return initFilter(ctx, {
        title: `Show only events concurrent to (causally unrelated to) event ${eventId}`,
        enabled: true,
        builtin: false,
        data: {},
        check: (_self, event) => event.id === refEvent.id || clockCmp(refEvent, event) === null,
    });
}
