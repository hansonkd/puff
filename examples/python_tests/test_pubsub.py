import asyncio

from puff import global_pubsub, spawn, sleep_ms
from asgiref.sync import async_to_sync, sync_to_async


pubsub = global_pubsub()


async def wait_for_message():
    conn = pubsub.connection()
    await conn.subscribe("test-chan")
    msg = await conn.receive()
    data = msg.json()
    return data["result"]


async def publish_and_wait():
    task = asyncio.create_task(wait_for_message())
    conn = pubsub.connection()
    await sleep_ms(100)
    await conn.publish_json("test-chan", {"result": 1})
    assert await task == 1


def wait_for_message_greenlet():
    conn = pubsub.connection()
    conn.subscribe("test-chan")
    msg = conn.receive()
    data = msg.json()
    return data["result"]


def publish_and_wait_greenlet():
    greenlet = spawn(wait_for_message_greenlet)
    conn = pubsub.connection()
    sleep_ms(100)
    conn.publish_json("test-chan", {"result": 2})
    assert greenlet.join() == 2


async def publish_and_wait_sync_from_async():
    task = asyncio.create_task(sync_to_async(wait_for_message_greenlet)())
    conn = pubsub.connection()
    await asyncio.sleep(0.1)
    await conn.publish_json("test-chan", {"result": 3})
    assert await task == 3


def wait_for_message_bytes():
    conn = pubsub.connection()
    conn.subscribe("test-chan")
    msg = conn.receive()
    return msg.body


def test_publish_and_wait_bytes():
    greenlet = spawn(wait_for_message_bytes)
    conn = pubsub.connection()
    sleep_ms(100)
    conn.publish_bytes("test-chan", b"123456")
    assert greenlet.join() == b"123456"


def wait_for_message_string():
    conn = pubsub.connection()
    conn.subscribe("test-chan")
    msg = conn.receive()
    return msg.text


def test_publish_and_wait_string():
    greenlet = spawn(wait_for_message_string)
    conn = pubsub.connection()
    sleep_ms(100)
    conn.publish("test-chan", "123456")
    assert greenlet.join() == "123456"


def wait_for_message_string_multi():
    conn = pubsub.connection()
    conn.subscribe("test-chan")
    msg = conn.receive()
    msg2 = conn.receive()
    return msg.text, msg2.text


def test_publish_and_wait_string_multi():
    greenlet = spawn(wait_for_message_string_multi)
    conn = pubsub.connection()
    sleep_ms(100)
    conn.publish("test-chan", "123456")
    conn.publish("test-chan", "789")
    assert greenlet.join() == ("123456", "789")


def wait_for_message_string_multi_channels():
    conn = pubsub.connection()
    conn.subscribe("test-chan-3")
    conn.subscribe("test-chan-2")
    conn.subscribe("test-chan-1")
    msg = conn.receive()
    msg2 = conn.receive()
    msg3 = conn.receive()
    return msg.text, msg2.text, msg3.text


def test_publish_and_wait_string_multi_channels():
    greenlet = spawn(wait_for_message_string_multi_channels)
    conn = pubsub.connection()
    sleep_ms(100)
    conn.publish("test-chan-1", "123456")
    conn.publish("test-chan-2", "789")
    conn.publish("test-chan-3", "abc")
    assert greenlet.join() == ("123456", "789", "abc")


def test_publish_and_wait_string_multi_channels_multi_wait():
    greenlet = spawn(wait_for_message_string_multi_channels)
    greenlet2 = spawn(wait_for_message_string_multi_channels)
    greenlet3 = spawn(wait_for_message_string_multi_channels)
    conn = pubsub.connection()
    sleep_ms(100)
    conn.publish("test-chan-1", "123456")
    conn.publish("test-chan-2", "789")
    conn.publish("test-chan-3", "abc")
    assert greenlet.join() == ("123456", "789", "abc")
    assert greenlet2.join() == ("123456", "789", "abc")
    assert greenlet3.join() == ("123456", "789", "abc")


def test_pubsub_async():
    async_to_sync(publish_and_wait)()


def test_pubsub_greenlet():
    publish_and_wait_greenlet()


def test_pubsub_sync_from_async():
    async_to_sync(publish_and_wait_sync_from_async)()
