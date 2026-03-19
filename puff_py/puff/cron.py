"""Cron-like task scheduling for Puff.

Uses Puff's existing task queue with periodic re-scheduling.
Each decorated function is automatically re-queued after execution.

Usage:
    from puff.cron import every, start_cron_jobs

    @every(minutes=5)
    def check_pending_orders(payload):
        # runs every 5 minutes
        ...

    @every(hours=1, name="hourly-cleanup")
    def cleanup(payload):
        ...

    # Call once at startup to kick off the first execution
    start_cron_jobs()
"""
import time
from puff.task_queue import global_task_queue


_cron_jobs = []


class CronJob:
    """Descriptor for a recurring task."""

    def __init__(self, name, interval_seconds, func):
        self.name = name
        self.interval_seconds = interval_seconds
        self.func = func
        self.func.__is_puff_task = True  # Required by task queue

    def schedule_next(self):
        """Schedule the next execution via the Puff task queue."""
        next_time = int(time.time() * 1000) + int(self.interval_seconds * 1000)
        global_task_queue.schedule_function(
            self.func,
            {},
            scheduled_time_unix_ms=next_time,
            timeout_ms=int(self.interval_seconds * 1000),
            keep_results_for_ms=60000,
        )


def every(seconds=None, minutes=None, hours=None, days=None, name=None):
    """Schedule a function to run at a fixed interval.

    At least one duration parameter must be provided.  They are additive:
    ``every(minutes=1, seconds=30)`` means every 90 seconds.

    The decorated function receives a single ``payload`` dict argument
    (always ``{}`` for cron jobs).
    """
    total_seconds = (
        (seconds or 0)
        + (minutes or 0) * 60
        + (hours or 0) * 3600
        + (days or 0) * 86400
    )

    def decorator(func):
        job_name = name or func.__name__
        func.__is_puff_task = True

        # Wrap to auto-reschedule after execution
        original_func = func

        def recurring_wrapper(payload):
            try:
                result = original_func(payload)
            finally:
                # Schedule next run regardless of success/failure
                job = CronJob(job_name, total_seconds, recurring_wrapper)
                job.schedule_next()
            return result

        recurring_wrapper.__name__ = func.__name__
        recurring_wrapper.__module__ = func.__module__
        recurring_wrapper.__qualname__ = func.__qualname__
        recurring_wrapper.__is_puff_task = True
        recurring_wrapper.__puff_cron_interval__ = total_seconds

        _cron_jobs.append(CronJob(job_name, total_seconds, recurring_wrapper))
        return recurring_wrapper

    return decorator


def start_cron_jobs():
    """Start all registered cron jobs.  Call once at application startup."""
    for job in _cron_jobs:
        print(f"Scheduling cron job: {job.name} (every {job.interval_seconds}s)")
        job.schedule_next()
