import { clockCmp, Ordering } from "../common/clocks.mts";
import { TaskId } from "../common/tasks.mts";
import { EventUI } from "./events.mts";
import { ExtContextUI } from "./ui.mts";

export function updateCdag(ctx: ExtContextUI) {
    // TODO: this is still a little bit broken, but it is probably because the
    // clocks coming from Shuttle are not quite right

    // find position of events in causal graph (CDAG)
    // TODO: only do once

    // in the CDAG we want to organise events vertically into tasks (same as
    // for the timeline view), and horizontally according to causality, with
    // program order represented as the order of events in a lane, and causal
    // dependence shown as arrows

    // for each task, keep track of highest clock it has seen so far
    // at each step; there can be a bump to the clock for one of two reasons:
    // - the task itself has performed an observable step
    // - another task has performed an observable step, and the current task
    //   causally depends on that other task

    // for display purposes, the first case is not relevant: the bump by
    // itself is only observable by the current task?

    const lastEvent: Map<TaskId, EventUI> = new Map();
    const lastSeenIndex: Map<TaskId, number> = new Map();
    const lastDependedOn: Map<TaskId, EventUI | null> = new Map();
    const taskColumn: Map<TaskId, number> = new Map();
    const columnUsed: Map<number, number> = new Map();
    for (let eventId = 0; eventId < ctx!.events.length; eventId++) {
        const event = ctx!.events[eventId];
        event.causallyDependsOn.length = 0;

        // the first event is in the first column
        let cdagIdx = 0;
        if (eventId === 0) {
            cdagIdx = 0;
        }

        // in the absence of causal ordering, each task starts in the first column
        if (!taskColumn.has(event.event.taskId)) {
            taskColumn.set(event.event.taskId, 0);
        }

        // look at the events of other tasks that happened since this task's last event
        if (!lastSeenIndex.has(event.event.taskId)) {
            lastSeenIndex.set(event.event.taskId, 0);
            lastDependedOn.set(event.event.taskId, null);
        }

        //console.log("checking", event.event.id, "of task", event.event.taskId, "range is from/to", lastSeenIndex.get(event.event.taskId), eventId);
        if (event.event.clock) {
            const dependingOn: Map<TaskId, EventUI> = new Map();
            for (let otherEventId = lastSeenIndex.get(event.event.taskId)!; otherEventId < eventId; otherEventId++) {
                const otherEvent = ctx!.events[otherEventId];

                if (otherEvent.event.taskId === event.event.taskId) {
                    cdagIdx = Math.max(cdagIdx, otherEvent.cdagIdx + 1);
                } else if (otherEvent.event.clock) {
                    // is there a causal ordering?
                    const order = clockCmp(otherEvent.event, event.event);
                    if (order === Ordering.Less) { // otherEvent happens-before event
                        //let transitiveDependency = false;

                        // TODO: remove, this shouldn't happen
                        // has the previous event in this task already causally dependend on otherEvent?
                        // if so, we don't want to add another link, it would be redundant
                        const prevEventOfThisTask = lastEvent.get(event.event.taskId);
                        if (prevEventOfThisTask && clockCmp(otherEvent.event, prevEventOfThisTask.event) === Ordering.Less) {
                            // console.log("  this shouldn't happen?", otherEvent.event.id, event.event.id, otherEvent.event.clock, event.event.clock);
                            continue;
                        }

                        //console.log("  less", otherEvent.event.id, event.event.id, otherEvent.event.clock, event.event.clock);

                        cdagIdx = Math.max(cdagIdx, otherEvent.cdagIdx + 1);
                        dependingOn.set(otherEvent.event.taskId, otherEvent);
                    } else if (order === Ordering.Equal) { // otherEvent === event?
                        //console.log("  equal???", otherEvent.event.id, event.event.id, otherEvent.event.clock, event.event.clock);
                    } else if (order === Ordering.Greater) { // this should not happen! we are iterating the orders in timeline order
                        //console.log("  greater?!", otherEvent.event.id, event.event.id, otherEvent.event.clock, event.event.clock);
                    } else { // no causal ordering
                        // pass
                    }
                }
            }
            for (const [_taskId, otherEvent] of dependingOn) {
                event.causallyDependsOn.push(otherEvent);
            }
            lastEvent.set(event.event.taskId, event);
            lastSeenIndex.set(event.event.taskId, eventId);
            //console.log("  result", eventId, cdagIdx, event.event.clock);
        }
        event.cdagIdx = cdagIdx;
        if (!columnUsed.has(cdagIdx)) {
            columnUsed.set(cdagIdx, 0);
        }
        if (!event.filtered) {
            columnUsed.set(cdagIdx, columnUsed.get(cdagIdx)! + 1);
        }
    }

    // sort used columns and give ones which contain more than one non-filtered event increasing indices
    // see https://stackoverflow.com/a/51242261
    const sortedColumns = new Map([...columnUsed].sort((a, b) => a[0] - b[0]));
    let cdagColumns = 0;
    for (const [column, visibleEvents] of sortedColumns) {
        columnUsed.set(column, cdagColumns);
        if (visibleEvents > 0) {
            cdagColumns++;
        }
    }
    ctx!.timeline.cdagColumns = cdagColumns;

    // renumber based on columns which are not empty
    for (let eventId = 0; eventId < ctx!.events.length; eventId++) {
        const event = ctx!.events[eventId];
        event.cdagKeptIdx = columnUsed.get(event.cdagIdx)!;
    }
}
