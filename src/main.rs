extern crate libc;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write, Result};
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::mem::{transmute, zeroed, uninitialized};
use std::slice;
use std::usize;
use std::vec::Vec;
use std::cell::RefCell;
mod tcmu;


const UIO_NAME: &str = "/sys/class/uio/uio0/name";
const MAP_SIZE: &str = "/sys/class/uio/uio0/maps/map0/size";
const UIO: &str = "/dev/uio0";

fn check_uio() {
    let mut name = File::open(UIO_NAME).unwrap();
    let mut s = String::new();
    name.read_to_string(&mut s).unwrap();
    print!("name: {:?}\n", s);

    if !s.starts_with("tcm-user") {
        panic!();
    }
}

fn get_mmap_size() -> usize {
    let mut map_size = File::open(MAP_SIZE).unwrap();
    let mut s = String::new();
    map_size.read_to_string(&mut s).unwrap();
    print!("size: {}\n", s);

    usize::from_str_radix(&s[2..s.len() - 1], 16).unwrap()
}

fn handle(storage: &mut [u8], uio: &mut File, p: *mut u8) {
    let mb: &mut tcmu::tcmu_mailbox =
        unsafe { transmute::<_, *mut tcmu::tcmu_mailbox>(p).as_mut().unwrap() };
    let cmdr_off = unsafe { ptr::read_volatile(&mb.cmdr_off) };
    let cmdr_size = unsafe { ptr::read_volatile(&mb.cmdr_size) };

    let mut poller = Poller::new();
    poller.register(uio.as_raw_fd());
    let mut n_empty_loop = 0;
    loop {
        if do_cmd(storage, p, mb, cmdr_size, cmdr_off) > 0 {
            uio.write(&[0u8; 4]).unwrap();
            n_empty_loop = 0;
        } else {
            n_empty_loop += 1;
            if n_empty_loop > 100 {
                n_empty_loop = 0;
                poller.wait();
            }
        }
    }
}

const CHECK_CONDITION: u8 = 2;
const NO_SENSE: u8 = 0;

fn not_handled(ent: &mut tcmu::tcmu_cmd_entry) {
    let buf = unsafe { &mut ent.__bindgen_anon_1.rsp.as_mut().sense_buffer };
    for x in buf.iter_mut() {
        *x = 0
    }
    buf[0] = 0x70;
    buf[2] = 5; // illegal request
    buf[7] = 0xa;
    // additional sense; invalid command op code
    buf[12] = 0x24;
    buf[13] = 0;
}

fn sense(ent: &mut tcmu::tcmu_cmd_entry, sense_key: u8) {
    let buf = unsafe { &mut ent.__bindgen_anon_1.rsp.as_mut().sense_buffer };
    for x in buf.iter_mut() {
        *x = 0
    }
    buf[0] = 0x70;
    buf[2] = sense_key as i8;

}

struct DataBuffer<'a> {
    base: *mut u8,
    iov: &'a [tcmu::iovec],
    i: usize,
    pos: usize,
}

impl<'a> DataBuffer<'a> {
    fn new(ent: &'a tcmu::tcmu_cmd_entry, base: *mut u8) -> DataBuffer<'a> {
        let iov_cnt = unsafe { ent.__bindgen_anon_1.req.as_ref().iov_cnt as usize };
        let iov = unsafe {
            ent.__bindgen_anon_1.req.as_ref().iov.as_slice(
                iov_cnt as usize,
            )
        };
        DataBuffer {
            base: base,
            iov: iov,
            i: 0,
            pos: 0,
        }
    }

    fn write(&mut self, mut buf: &[u8]) -> Result<usize> {
        let mut n = 0;
        while buf.len() > 0 && self.i < self.iov.len() {
            let out = unsafe {
                slice::from_raw_parts_mut(
                    self.base.offset(self.iov[self.i].iov_base as isize),
                    self.iov[self.i].iov_len as usize,
                )
            };
            let out = &mut out[self.pos..];
            let l = if buf.len() >= out.len() {
                self.pos = 0;
                self.i += 1;
                out.len()
            } else {
                self.pos += buf.len();
                buf.len()
            };
            &mut out[..l].copy_from_slice(&buf[..l]);
            buf = &buf[l..];
            n += l;
        }
        Ok(n)
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < buf.len() && self.i < self.iov.len() {
            let iob = unsafe {
                &slice::from_raw_parts(
                    self.base.offset(self.iov[self.i].iov_base as isize),
                    self.iov[self.i].iov_len as usize,
                )
                    [self.pos..]
            };
            let l = if buf.len() - n >= iob.len() {
                self.pos = 0;
                self.i += 1;
                iob.len()
            } else {
                self.pos += buf.len() - n;
                buf.len() - n
            };
            &mut buf[n..n + l].copy_from_slice(&iob[..l]);
            n += l;
        }
        Ok(n)
    }
}

fn handle_inquiry_std(ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) {
    let mut buf = [0; 36];
    buf[2] = 0x5; //spc-3
    buf[3] = 2; // response data format. spec says set 2.
    buf[4] = 31; //length
    buf[7] = 2; // cmdque should be set ?
    for x in &mut buf[8..] {
        *x = 0x20;
    }
    for (s, d) in "mem".bytes().zip(&mut buf[8..16]) {
        *d = s;
    }
    for (s, d) in "0001".bytes().zip(&mut buf[32..]) {
        *d = s;
    }
    DataBuffer::new(ent, base).write(&buf).unwrap();
}

unsafe fn load_cdb(p: *const u8) -> [u8; 16] {
    let mut s: [u8; 16] = uninitialized();
    let l = match (*p >> 5) & 7 {
        0b000 => 6,
        0b001 | 0b010 => 10,
        0b100 => 16,
        0b101 => 12,
        _ => unimplemented!(),
    };
    &mut s[..l].copy_from_slice(slice::from_raw_parts(p, l));
    s
}

fn handle_inquiry(cdb: &[u8], base: *mut u8, ent: &mut tcmu::tcmu_cmd_entry) -> u8 {
    let evpd = cdb[1] & 1 > 0;
    if !evpd {
        if cdb[2] != 0 {
            not_handled(ent);
            CHECK_CONDITION
        } else {
            handle_inquiry_std(ent, base);
            NO_SENSE
        }
    } else {
        // not support evpd
        print!("evpd inquiry\n");
        not_handled(ent);
        CHECK_CONDITION
    }
}

fn handle_test_unit_ready() -> u8 {
    NO_SENSE
}

#[repr(C, packed)]
struct mode_parameter_header {
    mode_data_length: u8,
    medium_type: u8,
    wp_dpofua: u8,
    block_description_length: u8,
}

fn handle_mode_sense_6(cdb: &[u8], ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) -> u8 {
    let dbd = cdb[1] & (1 << 3) > 0;
    let pc = cdb[2] >> 6;
    let page_code = cdb[2] & 0x3f;
    let subpage_code = cdb[3];

    print!(
        "mode sense6: dbd {}, pc {}, page code {}, sub {}\n",
        dbd,
        pc,
        page_code,
        subpage_code
    );

    match (page_code, subpage_code) {
        (0x0, _) => {
            // vender specific.
            let header = mode_parameter_header {
                mode_data_length: 3,
                medium_type: 0,
                wp_dpofua: 1 << 4, //dpofua is 1, wp is 0
                block_description_length: 0,
            };
            let mut b = DataBuffer::new(ent, base);
            b.write(unsafe { transmute::<_, &[u8; 4]>(&header) })
                .unwrap();
            NO_SENSE
        }
        (0x3f, _) /* all pages */| ( 0x8, _) /* caching */=> {
            let header = mode_parameter_header {
                mode_data_length: 3 + 20,
                medium_type: 0,
                wp_dpofua: 1 << 4, //dpofua is 1, wp is 0
                block_description_length: 0,
            };
            let mut b= DataBuffer::new(ent, base);
            b.write(unsafe { transmute::<_, &[u8; 4]>(&header) })
                .unwrap();
            let mut cache_page = [0u8; 20];
            cache_page[0] = 0x8;
            cache_page[1] = 0x12;
            cache_page[2] = 1 << 2; //WCE, RCD
            b.write(&cache_page).unwrap();
            NO_SENSE
        }

        _ => {
            not_handled(ent);
            CHECK_CONDITION
        }
    }

}

const LBA: usize = 0xffffffff_fffffffe;
// In sd.c, 512, 1024, 2048, 4096 are only supported.
const BLOCK_SIZE: usize = 4096;

fn handle_read_capacity_10(ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) -> u8 {
    let lba = if LBA > 0xffffffff {
        0xffffffff
    } else {
        (LBA as u32).to_be()
    };
    let block_size = (BLOCK_SIZE as u32).to_be();
    let mut b = DataBuffer::new(ent, base);
    b.write(&unsafe { transmute::<_, [u8; 4]>(lba) }).unwrap();
    b.write(&unsafe { transmute::<_, [u8; 4]>(block_size) })
        .unwrap();

    NO_SENSE
}

fn handle_report_supported_operation_codes(
    cdb: &[u8],
    ent: &mut tcmu::tcmu_cmd_entry,
    base: *mut u8,
) -> u8 {
    let requested_operation_code = cdb[3];
    if cdb[2] == 1 {
        // RCTD is 0 and reporting options is 1
        let mut b = DataBuffer::new(ent, base);
        // partial impl. CDB usage data is omitted.
        let mut buf = [0u8; 2];
        match requested_operation_code {
            INQUIRY |
            TEST_UNIT_READY |
            MODE_SENSE_6 |
            READ_CAPACITY_10 |
            REPORT_OPERATION |
            SERVICE_ACTION_IN_16 |
            READ_10 |
            WRITE_10 |
            READ_16 |
            WRITE_16 => {
                buf[1] = 3;
            }
            _ => {
                print!("requested op code: {:x}\n", requested_operation_code);
                // not supported
                buf[1] = 1;
            }
        }
        b.write(&buf).unwrap();
        NO_SENSE
    } else {
        not_handled(ent);
        CHECK_CONDITION
    }
}

fn handle_report(cdb: &[u8], ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) -> u8 {
    let service_action = cdb[1] & 0x1f;
    match service_action {
        0xc => handle_report_supported_operation_codes(cdb, ent, base),
        _ => {
            not_handled(ent);
            CHECK_CONDITION
        }
    }
}

fn handle_read_capacity_16(cdb: &[u8], ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) -> u8 {
    let lba = (LBA as u64).to_be();
    let block_size = (BLOCK_SIZE as u32).to_be();
    let mut b = DataBuffer::new(ent, base);
    b.write(&unsafe { transmute::<_, [u8; 8]>(lba) }).unwrap();
    b.write(&unsafe { transmute::<_, [u8; 4]>(block_size) })
        .unwrap();
    let mut buf = [0u8; 32];
    buf[0] = 1 << 4; // rc basis. no zone (?)
    b.write(&buf).unwrap();
    NO_SENSE
}

fn handle_service_action_16(cdb: &[u8], ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) -> u8 {
    let service_action = cdb[1] & 0x1f;
    match service_action {
        0x10 => handle_read_capacity_16(cdb, ent, base),
        _ => {
            not_handled(ent);
            CHECK_CONDITION
        }
    }
}


fn handle_read_write_1x(
    storage: &mut [u8],
    cdb: &[u8],
    ent: &mut tcmu::tcmu_cmd_entry,
    base: *mut u8,
) -> u8 {
    let _protect = cdb[1] >> 5;
    let _dpo = (cdb[1] >> 4) & 1;
    let _fua = (cdb[1] >> 3) & 1;
    let _rarc = (cdb[1] >> 2) & 1;
    let lba = match cdb[0] {
        READ_10 | WRITE_10 => unsafe {
            u32::from_be(*transmute::<_, *const u32>(&cdb[2])) as usize
        },
        READ_16 | WRITE_16 => unsafe {
            u64::from_be(*transmute::<_, *const u64>(&cdb[2])) as usize
        },
        _ => unreachable!(),
    };
    let transfer_length = match cdb[0] {
        READ_10 | WRITE_10 => unsafe {
            u16::from_be(*transmute::<_, *const u16>(&cdb[7])) as usize
        },
        READ_16 | WRITE_16 => unsafe {
            u32::from_be(*transmute::<_, *const u32>(&cdb[10])) as usize
        },
        _ => unreachable!(),
    };

    let begin = BLOCK_SIZE * lba;
    let end = begin + BLOCK_SIZE * transfer_length;
    let mut b = DataBuffer::new(ent, base);
    match cdb[0] {
        READ_10 | READ_16 => // b.write(&storage[begin..end]).unwrap()
        {},
        WRITE_10 | WRITE_16 => // b.read(&mut storage[begin..end]).unwrap()
        {}  ,
        _ => unreachable!(),
    };

    NO_SENSE
}

const READ_10: u8 = 0x28;
const WRITE_10: u8 = 0x2a;
const READ_16: u8 = 0x88;
const WRITE_16: u8 = 0x8a;

const INQUIRY: u8 = 0x12;
const TEST_UNIT_READY: u8 = 0x00;
const MODE_SENSE_6: u8 = 0x1a;
const READ_CAPACITY_10: u8 = 0x25;
const SERVICE_ACTION_IN_16: u8 = 0x9e;
const REPORT_OPERATION: u8 = 0xa3;

const SYNCHRONIZE_CACHE_10: u8 = 0x35;

fn do_cmd(
    storage: &mut [u8],
    p: *mut u8,
    mb: &mut tcmu::tcmu_mailbox,
    cmdr_size: u32,
    cmdr_off: u32,
) -> usize {
    // todo?: use Atomic to load cmd_head
    let cmd_head = unsafe { ptr::read_volatile(&mb.cmd_head) };
    let mut cmd_tail = unsafe { ptr::read_volatile(&mb.cmd_tail) };
    let head_p = unsafe { p.offset((cmdr_off + cmd_head) as isize) };
    let mut ent_p = unsafe { p.offset((cmdr_off + cmd_tail) as isize) };
    #[cfg(debug_assertions)]
    print!("cmd_head {}\n", cmd_head);
    #[cfg(debug_assertions)]
    print!("cmd_tail {}\n", cmd_tail);
    let mut n_proceeded = 0;
    while ent_p != head_p {
        let ent = unsafe { (ent_p as *mut tcmu::tcmu_cmd_entry).as_mut().unwrap() };
        let mut local_ent = unsafe { ptr::read_volatile(ent) };
        let local_ent_p = &mut local_ent;
        let op = tcmu::tcmu_hdr_get_op(local_ent_p.hdr.len_op);
        #[cfg(debug_assertions)]
        print!(
            "op: {} id: {} k: {} u: {}\n",
            op,
            local_ent_p.hdr.cmd_id,
            local_ent_p.hdr.kflags,
            local_ent_p.hdr.uflags
        );
        if op == tcmu::tcmu_opcode::TCMU_OP_CMD as u32 {
            unsafe {
                let cdb_p = p.offset(local_ent_p.__bindgen_anon_1.req.as_ref().cdb_off as isize);
                let cdb = &load_cdb(cdb_p);
                let command = cdb[0];
                let status = match command {
                    INQUIRY => handle_inquiry(cdb, p, local_ent_p),
                    TEST_UNIT_READY => handle_test_unit_ready(),
                    MODE_SENSE_6 => handle_mode_sense_6(cdb, local_ent_p, p),
                    READ_CAPACITY_10 => handle_read_capacity_10(local_ent_p, p),
                    REPORT_OPERATION => handle_report(cdb, local_ent_p, p),
                    READ_10 | WRITE_10 | READ_16 | WRITE_16 => {
                        handle_read_write_1x(storage, cdb, local_ent_p, p)
                    }
                    SERVICE_ACTION_IN_16 => handle_service_action_16(cdb, local_ent_p, p),
                    SYNCHRONIZE_CACHE_10 => {
                        //todo
                        NO_SENSE
                    }
                    _ => {
                        print!("Unsupported SCSI opcode: 0x{:x}\n", command);
                        not_handled(local_ent_p);
                        CHECK_CONDITION
                    }
                };
                if status == NO_SENSE {
                    ptr::write_volatile(&mut ent.__bindgen_anon_1.rsp.as_mut().scsi_status, status);
                } else {
                    local_ent_p.__bindgen_anon_1.rsp.as_mut().scsi_status = status;
                    ptr::write_volatile(ent, *local_ent_p);
                }
            }
        } else if op == tcmu::tcmu_opcode::TCMU_OP_PAD as u32 {
            // do nothing
        } else {
            panic!("unknown cmd: {}", op);
        }
        cmd_tail = (cmd_tail + tcmu::tcmu_hdr_get_len(local_ent_p.hdr.len_op)) % cmdr_size;
        #[cfg(debug_assertions)]
        print!("cmd_tail {}\n", cmd_tail);
        ent_p = unsafe { p.offset((cmdr_off + cmd_tail) as isize) };
        n_proceeded += 1;
    }
    unsafe {
        ptr::write_volatile(&mut mb.cmd_tail, cmd_tail);
    }
    n_proceeded
}


struct Poller {
    eventfd: libc::c_int,
    events: RefCell<Vec<libc::epoll_event>>,
}

impl Poller {
    fn new() -> Poller {
        unsafe {
            let fd = libc::epoll_create(1 /*dummy*/);
            if fd == -1 {
                panic!("epoll_create failed");
            }
            Poller {
                eventfd: fd,
                events: RefCell::new(Vec::new()),
            }
        }
    }

    fn register(&mut self, fd: libc::c_int) {
        let mut ev = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLET) as u32,
            u64: fd as u64,
        };
        unsafe {
            if libc::epoll_ctl(self.eventfd, libc::EPOLL_CTL_ADD, fd, &mut ev) == -1 {
                panic!("epoll_ctl failed");
            }
        }
        let events = self.events.get_mut();
        let n = events.len() + 1;
        events.resize(n, unsafe { zeroed() });
    }

    fn wait(&self) {
        let mut events = self.events.borrow_mut();
        unsafe {
            libc::epoll_wait(
                self.eventfd,
                events.as_mut_slice().as_mut_ptr(),
                events.len() as i32,
                -1,
            );
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.eventfd);
        }
    }
}

fn main() {
    check_uio();

    let mmap_size = get_mmap_size();

    let mut uio = OpenOptions::new().read(true).write(true).open(UIO).unwrap();
    let p = unsafe {
        let p = libc::mmap(
            ptr::null_mut(),
            mmap_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            uio.as_raw_fd(),
            0,
        );
        if p == transmute(-1 as i64) {
            panic!();
        }
        p as *mut u8
    };

    // let mut storage = Vec::with_capacity(DEVICE_SIZE);
    // unsafe {
    //     storage.set_len(DEVICE_SIZE);
    // }
    let mut storage = Vec::new();
    handle(&mut storage[..], &mut uio, p);
}
