from puff.task_queue import global_task_queue

task_queue = global_task_queue()


def test_task_queue_async_sync():
    all_tasks = []
    for x in range(100):
        task1 = task_queue.schedule_function(
            example_task,
            {"type": "coroutine", "x": [x]},
            timeout_ms=100,
            keep_results_for_ms=5 * 1000,
        )
        task2 = task_queue.schedule_function(
            example_task_async, {"type": "async", "x": [x]}, scheduled_time_unix_ms=1
        )
        all_tasks.append((f"coroutine-{x}", task1))
        all_tasks.append((f"async-{x}", task2))

    for (expected, task) in all_tasks:
        result = task_queue.wait_for_task_result(task)
        assert result == expected


def example_task(payload):
    assert payload["type"] == "coroutine"
    return f"coroutine-{payload['x'][0]}"


async def example_task_async(payload):
    assert payload["type"] == "async"
    return f"async-{payload['x'][0]}"
