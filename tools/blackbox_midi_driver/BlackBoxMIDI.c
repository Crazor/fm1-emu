// A CoreMIDI driver with one device, "BlackBox": the emulated ANN Blackbox as a real MIDI device.
//
// M-VAVE's M-EFCS only lists native CoreMIDI devices (a virtual endpoint has no device) with the
// name "BlackBox", and reads replies as a byte stream, so a looping port such as an IAC bus feeds
// it its own request. This driver gives it a real device with a separate source and destination.
//
// It runs inside MIDIServer and talks to `fm1-live --usb-bridge` over TCP on 127.0.0.1: the driver
// listens on 47811 and serves one client at a time (a new connection replaces the old one). Each
// MIDI message is a frame, its length as 4 bytes little-endian and then its bytes. With no client
// the device is there but offline (kMIDIPropertyOffline), and MIDI sent to it goes nowhere.
//
// One thread does all the socket work. Send only copies frames to a bounded queue, so a client
// that stops reading costs dropped MIDI and never holds up MIDIServer.
//
// SPDX-License-Identifier: MIT

#include <CoreFoundation/CoreFoundation.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <CoreMIDI/CoreMIDI.h>
#include <CoreMIDI/MIDIDriver.h>
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define DRIVER_PORT 47811
// The longest MIDI message either way, SysEx included: fm1-live's MAX_MESSAGE, the same value. Far
// beyond any real SysEx dump, and a limit on what a bad length prefix can make us wait for.
#define MAX_MESSAGE (1u << 20)
#define FRAME_HEADER 4
// Frames waiting for a slow client; when full, new messages are dropped whole.
#define TX_CAPACITY (4 * (MAX_MESSAGE + FRAME_HEADER))
// A MIDIPacket's length is 16 bits, but a whole MIDIPacketList may be at most 65536 bytes
// (MIDIServices.h), headers included: stay below that and send longer messages in several.
#define PACKET_LIST_SIZE 65536
#define MAX_PACKET_DATA 65000

// Info.plist's CFPlugInFactories key.
#define FACTORY_UUID                                                                               \
    CFUUIDGetConstantUUIDWithBytes(NULL, 0x7B, 0x1C, 0x3E, 0x5A, 0x4D, 0x2F, 0x4A, 0x8E, 0x9C,     \
                                   0x61, 0x3F, 0x0A, 0x8D, 0x5E, 0x2B, 0x71)

typedef struct {
    MIDIDriverInterface *vtbl; // first: a MIDIDriverRef points here
    UInt32 refs;
    MIDIDeviceRef device;
    MIDIEndpointRef source;
    int listener;
    int wake[2]; // a pipe: tells the socket thread that it has something to do
    atomic_bool running;
    pthread_t thread;
    uint8_t sysex[MAX_MESSAGE];
    size_t sysex_len;
    bool in_sysex, sysex_overflow;

    pthread_mutex_t lock; // guards client (written only by the socket thread) and the queue
    int client;
    uint8_t tx[TX_CAPACITY];
    size_t tx_start, tx_end;

    // the socket thread's own
    uint8_t rx[FRAME_HEADER + MAX_MESSAGE];
    size_t rx_len;
    union {
        MIDIPacketList list;
        uint8_t bytes[PACKET_LIST_SIZE];
    } packets;
} Driver;

static Driver *of(void *self) { return (Driver *)self; }

static void set_offline(Driver *d, bool offline) {
    MIDIObjectSetIntegerProperty(d->device, kMIDIPropertyOffline, offline);
}

// (from Send: never waits for the socket)
static void to_emulator(Driver *d, const uint8_t *msg, size_t n) {
    pthread_mutex_lock(&d->lock);
    size_t waiting = d->tx_end - d->tx_start;
    if (d->client >= 0 && waiting + FRAME_HEADER + n <= TX_CAPACITY) {
        if (d->tx_end + FRAME_HEADER + n > TX_CAPACITY) {
            memmove(d->tx, d->tx + d->tx_start, waiting);
            d->tx_start = 0;
            d->tx_end = waiting;
        }
        uint8_t *at = d->tx + d->tx_end;
        at[0] = n & 0xff;
        at[1] = (n >> 8) & 0xff;
        at[2] = (n >> 16) & 0xff;
        at[3] = (n >> 24) & 0xff;
        memcpy(at + FRAME_HEADER, msg, n);
        d->tx_end += FRAME_HEADER + n;
        if (!waiting) write(d->wake[1], "", 1); // (non-blocking: if full, a wake-up is pending anyway)
    }
    pthread_mutex_unlock(&d->lock);
}

static size_t message_length(uint8_t status) {
    if (status < 0xf0) return (status & 0xf0) == 0xc0 || (status & 0xf0) == 0xd0 ? 2 : 3;
    if (status == 0xf1 || status == 0xf3) return 2;
    return status == 0xf2 ? 3 : 1;
}

// A packet holds whole messages or a piece of a SysEx: split them, and gather the SysEx.
static void feed(Driver *d, const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n;) {
        uint8_t b = p[i];
        if (b >= 0xf8) { // real time, even inside a SysEx
            to_emulator(d, &b, 1);
            i++;
        } else if (d->in_sysex || b == 0xf0) {
            d->in_sysex = true;
            if (d->sysex_len < MAX_MESSAGE) d->sysex[d->sysex_len++] = b;
            else d->sysex_overflow = true;
            i++;
            if (b == 0xf7) {
                if (!d->sysex_overflow) to_emulator(d, d->sysex, d->sysex_len);
                d->sysex_len = 0;
                d->in_sysex = d->sysex_overflow = false;
            }
        } else if (b < 0x80) {
            i++; // a stray data byte
        } else {
            size_t len = message_length(b);
            if (len > n - i) len = n - i;
            to_emulator(d, p + i, len);
            i += len;
        }
    }
}

// A message for the MIDI clients, in as many packets as it takes (a SysEx may span several).
static void to_midi(Driver *d, const uint8_t *msg, size_t n) {
    for (size_t at = 0; at < n;) {
        size_t chunk = n - at > MAX_PACKET_DATA ? MAX_PACKET_DATA : n - at;
        MIDIPacketList *pl = &d->packets.list;
        MIDIPacket *p = MIDIPacketListInit(pl);
        p = MIDIPacketListAdd(pl, PACKET_LIST_SIZE, p, 0, (ByteCount)chunk, msg + at);
        if (!p) return; // (cannot happen at this size; a SysEx with a hole would be worse than none)
        MIDIReceived(d->source, pl);
        at += chunk;
    }
}

static void forget_client_data(Driver *d) {
    d->tx_start = d->tx_end = 0;
    d->rx_len = 0;
}

static void drop_client(Driver *d) {
    pthread_mutex_lock(&d->lock);
    close(d->client);
    d->client = -1;
    forget_client_data(d);
    pthread_mutex_unlock(&d->lock);
    set_offline(d, true);
}

static bool configure_client(int fd) {
    int on = 1;
    int flags = fcntl(fd, F_GETFL);
    return flags != -1 && fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0 &&
           setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on) == 0 && // MIDIServer must survive a dead client
           setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on) == 0;
}

static void accept_clients(Driver *d) {
    for (;;) {
        int fd = accept(d->listener, NULL, NULL);
        if (fd < 0) {
            if (errno == EINTR || errno == ECONNABORTED) continue;
            if (errno != EAGAIN) usleep(100000); // (out of descriptors, say: do not spin on poll)
            return;
        }
        if (!configure_client(fd)) {
            close(fd);
            continue;
        }
        pthread_mutex_lock(&d->lock);
        int old = d->client;
        d->client = fd;
        forget_client_data(d);
        pthread_mutex_unlock(&d->lock);
        if (old >= 0) close(old);
        else set_offline(d, false);
    }
}

// false: the connection is finished.
static bool read_client(Driver *d) {
    ssize_t n = recv(d->client, d->rx + d->rx_len, sizeof d->rx - d->rx_len, 0);
    if (n == 0) return false;
    if (n < 0) return errno == EAGAIN || errno == EINTR;
    d->rx_len += n;
    size_t at = 0;
    while (d->rx_len - at >= FRAME_HEADER) {
        const uint8_t *h = d->rx + at;
        uint32_t len = h[0] | h[1] << 8 | h[2] << 16 | (uint32_t)h[3] << 24;
        if (len > MAX_MESSAGE) return false; // no way to find the next frame
        if (d->rx_len - at - FRAME_HEADER < len) break;
        to_midi(d, h + FRAME_HEADER, len);
        at += FRAME_HEADER + len;
    }
    if (at) {
        memmove(d->rx, d->rx + at, d->rx_len - at);
        d->rx_len -= at;
    }
    return true;
}

// As much of the queue as the socket takes now. false: the connection is finished.
static bool write_client(Driver *d) {
    bool alive = true;
    pthread_mutex_lock(&d->lock);
    while (d->tx_start < d->tx_end) {
        ssize_t n = send(d->client, d->tx + d->tx_start, d->tx_end - d->tx_start, 0);
        if (n >= 0) {
            d->tx_start += n;
        } else if (errno != EINTR) {
            alive = errno == EAGAIN;
            break;
        }
    }
    if (d->tx_start == d->tx_end) d->tx_start = d->tx_end = 0;
    pthread_mutex_unlock(&d->lock);
    return alive;
}

static void *socket_loop(void *arg) {
    Driver *d = arg;
    while (d->running) {
        struct pollfd fds[3] = {{.fd = d->listener, .events = POLLIN},
                                {.fd = d->wake[0], .events = POLLIN},
                                {.fd = d->client, .events = POLLIN}}; // (poll skips a client of -1)
        pthread_mutex_lock(&d->lock);
        if (d->tx_start < d->tx_end) fds[2].events |= POLLOUT;
        pthread_mutex_unlock(&d->lock);
        if (poll(fds, 3, -1) < 0) {
            if (errno != EINTR) usleep(100000);
            continue;
        }
        if (fds[1].revents) {
            char junk[64];
            while (read(d->wake[0], junk, sizeof junk) > 0) {}
        }
        if (fds[0].revents) accept_clients(d);
        // (a client just replaced has a different descriptor: its results are stale)
        if (d->client >= 0 && d->client == fds[2].fd && fds[2].revents & (POLLIN | POLLHUP | POLLERR | POLLNVAL))
            if (!read_client(d)) drop_client(d);
        if (d->client >= 0 && !write_client(d)) drop_client(d);
    }
    return NULL;
}

static HRESULT QueryInterface(void *self, REFIID iid, LPVOID *out) {
    CFUUIDRef asked = CFUUIDCreateFromUUIDBytes(NULL, iid);
    bool ok = CFEqual(asked, IUnknownUUID) || CFEqual(asked, kMIDIDriverInterfaceID) ||
              CFEqual(asked, kMIDIDriverInterface2ID);
    CFRelease(asked);
    if (!ok) {
        *out = NULL;
        return E_NOINTERFACE;
    }
    of(self)->refs++;
    *out = self;
    return S_OK;
}

static ULONG AddRef(void *self) { return ++of(self)->refs; }

static ULONG Release(void *self) {
    Driver *d = of(self);
    if (--d->refs) return d->refs;
    CFPlugInRemoveInstanceForFactory(FACTORY_UUID);
    pthread_mutex_destroy(&d->lock);
    free(d);
    return 0;
}

static OSStatus FindDevices(MIDIDriverRef self, MIDIDeviceListRef devices) { return noErr; }

static void close_sockets(Driver *d) {
    pthread_mutex_lock(&d->lock);
    if (d->client >= 0) close(d->client);
    if (d->wake[0] >= 0) close(d->wake[0]);
    if (d->wake[1] >= 0) close(d->wake[1]);
    d->client = d->wake[0] = d->wake[1] = -1;
    forget_client_data(d);
    pthread_mutex_unlock(&d->lock);
    if (d->listener >= 0) close(d->listener);
    d->listener = -1;
}

static OSStatus Start(MIDIDriverRef self, MIDIDeviceListRef devices) {
    Driver *d = of(self);
    // the device from an earlier run stays in the MIDI setup: reuse it
    MIDIDeviceListRef mine = MIDIGetDriverDeviceList(self);
    MIDIDeviceRef device = MIDIDeviceListGetNumberOfDevices(mine) ? MIDIDeviceListGetDevice(mine, 0) : 0;
    MIDIDeviceListDispose(mine);
    MIDIEntityRef entity = 0;
    if (device) {
        entity = MIDIDeviceGetEntity(device, 0);
    } else {
        // (device and entity share the name: the endpoint's display name is then just "BlackBox")
        OSStatus err = MIDIDeviceCreate(self, CFSTR("BlackBox"), CFSTR("M-VAVE"), CFSTR("BlackBox"), &device);
        if (err) return err;
        err = MIDIDeviceAddEntity(device, CFSTR("BlackBox"), false, 1, 1, &entity);
        if (err) return err;
        err = MIDISetupAddDevice(device);
        if (err) return err;
    }
    d->source = MIDIEntityGetSource(entity, 0);
    d->device = device;
    set_offline(d, true); // until a client connects

    if (pipe(d->wake)) {
        d->wake[0] = d->wake[1] = -1;
        return kMIDINoConnection;
    }
    fcntl(d->wake[0], F_SETFL, O_NONBLOCK);
    fcntl(d->wake[1], F_SETFL, O_NONBLOCK);
    d->listener = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in at = {.sin_len = sizeof at, .sin_family = AF_INET,
                             .sin_port = htons(DRIVER_PORT), .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    int on = 1;
    // (SO_REUSEADDR: a restarted MIDIServer must get the port back while the old connection lingers)
    if (d->listener < 0 || setsockopt(d->listener, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on) ||
        fcntl(d->listener, F_SETFL, O_NONBLOCK) || bind(d->listener, (struct sockaddr *)&at, sizeof at) ||
        listen(d->listener, 4)) {
        close_sockets(d);
        return kMIDINoConnection;
    }
    d->running = true;
    if (pthread_create(&d->thread, NULL, socket_loop, d)) {
        d->running = false;
        close_sockets(d);
        return kMIDINoConnection;
    }
    return noErr;
}

static OSStatus Stop(MIDIDriverRef self) {
    Driver *d = of(self);
    if (d->running) {
        d->running = false;
        write(d->wake[1], "", 1);
        pthread_join(d->thread, NULL);
    }
    close_sockets(d);
    return noErr;
}

static OSStatus Configure(MIDIDriverRef self, MIDIDeviceRef device) { return noErr; }

static OSStatus Send(MIDIDriverRef self, const MIDIPacketList *list, void *ref1, void *ref2) {
    const MIDIPacket *p = &list->packet[0];
    for (UInt32 i = 0; i < list->numPackets; i++) {
        feed(of(self), p->data, p->length);
        p = MIDIPacketNext(p);
    }
    return noErr;
}

static OSStatus EnableSource(MIDIDriverRef self, MIDIEndpointRef src, Boolean on) { return noErr; }
static OSStatus Flush(MIDIDriverRef self, MIDIEndpointRef dest, void *ref1, void *ref2) { return noErr; }
static OSStatus Monitor(MIDIDriverRef self, MIDIEndpointRef dest, const MIDIPacketList *list) { return noErr; }

static MIDIDriverInterface vtable = {
    .QueryInterface = QueryInterface,
    .AddRef = AddRef,
    .Release = Release,
    .FindDevices = FindDevices,
    .Start = Start,
    .Stop = Stop,
    .Configure = Configure,
    .Send = Send,
    .EnableSource = EnableSource,
    .Flush = Flush,
    .Monitor = Monitor,
};

__attribute__((visibility("default")))
void *NewBlackBoxDriver(CFAllocatorRef allocator, CFUUIDRef type) {
    if (!CFEqual(type, kMIDIDriverTypeID)) return NULL;
    Driver *d = calloc(1, sizeof *d);
    d->vtbl = &vtable;
    d->refs = 1;
    d->listener = d->client = d->wake[0] = d->wake[1] = -1;
    pthread_mutex_init(&d->lock, NULL);
    CFPlugInAddInstanceForFactory(FACTORY_UUID);
    return d;
}
