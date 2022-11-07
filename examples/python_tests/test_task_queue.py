from puff.task_queue import global_task_queue
from puff.pubsub import global_pubsub
from puff.redis import global_redis
from puff.json import loadb, dumpb
import secrets

task_queue = global_task_queue
pubsub = global_pubsub
redis = global_redis


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


def test_task_queue_pubsub_realtime():
    conn = pubsub.connection()
    this_channel = secrets.token_hex(6)
    conn.subscribe(this_channel)
    task_queue.schedule_function(
        example_task_pubsub_async, {"channel": this_channel, "x": [42]}
    )
    result = conn.receive()
    data = result.json()
    assert data["my_result"] == "pubsub-42"


async def example_task_pubsub_async(payload):
    channel = payload["channel"]
    await pubsub.publish_json_as(
        pubsub.new_connection_id(), channel, {"my_result": f"pubsub-{payload['x'][0]}"}
    )
    return None


def test_task_queue_lpop_realtime():
    this_channel = secrets.token_hex(6)
    task_queue.schedule_function(
        example_task_lpush_async, {"channel": this_channel, "x": [42]}
    )
    # Wait for the result.
    result = redis.blpop(this_channel.encode("utf8"), 0)
    data = result and loadb(result[1])
    assert data["my_result"] == "lpop-42"


async def example_task_lpush_async(payload):
    channel = payload["channel"]
    await redis.lpush(
        channel.encode("utf8"), dumpb({"my_result": f"lpop-{payload['x'][0]}"})
    )
    return None
