import { Event } from "./events.mts";

export enum Ordering {
    Less = -1,
    Equal,
    Greater,
}

function unifyOrdering(a: Ordering, b: Ordering): Ordering | null {
    if (a === Ordering.Equal && b === Ordering.Equal) {
        return Ordering.Equal;
    } else if ((a === Ordering.Less && b === Ordering.Greater)
        || (a === Ordering.Greater && b === Ordering.Less)) {
        return null;
    } else if (a === Ordering.Less || b === Ordering.Less) {
        return Ordering.Less;
    } else if (a === Ordering.Greater || b === Ordering.Greater) {
        return Ordering.Greater;
    } else {
        // unreachable!
        return null;
    }
}

function numCmp(a: number, b: number): Ordering {
    return a < b ? Ordering.Less : (a > b ? Ordering.Greater : Ordering.Equal);
}

export function clockCmp({clock: clockA}: Event, {clock: clockB}: Event): Ordering | null {
    if (clockA === null || clockB === null) {
        return null;
    }
    const entriesA = clockA.length;
    const entriesB = clockB.length;
    let ord: Ordering | null = numCmp(entriesA, entriesB);
    for (let i = 0; i < Math.min(entriesA, entriesB); i++) {
        ord = unifyOrdering(ord, numCmp(clockA[i], clockB[i]));
        if (ord === null) {
            return null;
        }
    }
    return ord;
}
