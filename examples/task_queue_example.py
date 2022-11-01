from puff.task_queue import global_task_queue

task_queue = global_task_queue()


def run_main():
    all_tasks = []
    for x in range(100):
        task1 = task_queue.schedule_function(
            my_awesome_task,
            {"type": "coroutine", "x": [x]},
            timeout_ms=100,
            keep_results_for_ms=5 * 1000,
        )
        #  override `scheduled_time_unix_ms` so that async tasks execute with priority over the coroutine tasks.
        #  Since they have the same priority, they may be executed out of the order they were scheduled.
        task2 = task_queue.schedule_function(
            my_awesome_task_async, {"type": "async", "x": [x]}, scheduled_time_unix_ms=1
        )
        #  These tasks will keep their order since their priorities in `scheduled_time_unix_ms` match their order.
        task3 = task_queue.schedule_function(
            my_awesome_task_async,
            {"type": "async-ordered", "x": [x]},
            scheduled_time_unix_ms=x,
        )
        print(f"Put tasks {task1}, {task2}, {task3} in queue")
        all_tasks.append(task1)
        all_tasks.append(task2)
        all_tasks.append(task3)

    for task in all_tasks:
        result = task_queue.wait_for_task_result(task, 100, 1000)
        print(f"{task} returned {result}")


def my_awesome_task(payload):
    print(f"In task {payload}")
    return payload["x"][0]


async def my_awesome_task_async(payload):
    print(f"In async task {payload}")
    return payload["x"][0]
