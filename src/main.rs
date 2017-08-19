extern crate libc;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::mem::transmute;
use std::slice;
use std::usize;
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

fn handle(uio: &mut File, p: *mut u8) {
    loop {
        print!("begin\n");
        let mut dummy: [u8; 4] = [0, 0, 0, 0];
        uio.read(&mut dummy).unwrap();
        do_cmd(p);
        dummy = [0, 0, 0, 0];
        uio.write(&dummy).unwrap();
        print!("done\n");
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

struct Responder<'a> {
    base: *mut u8,
    iov: &'a [tcmu::iovec],
    i: usize,
    pos: usize,
}

impl<'a> Responder<'a> {
    fn new(ent: &'a tcmu::tcmu_cmd_entry, base: *mut u8) -> Responder<'a> {
        let iov_cnt = unsafe { ent.__bindgen_anon_1.req.as_ref().iov_cnt as usize };
        let iov = unsafe {
            ent.__bindgen_anon_1.req.as_ref().iov.as_slice(
                iov_cnt as usize,
            )
        };
        Responder {
            base: base,
            iov: iov,
            i: 0,
            pos: 0,
        }
    }

    fn write(&mut self, mut buf: &[u8]) {
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
        }
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
    Responder::new(ent, base).write(&buf);
}

unsafe fn into_cdb6<'a>(p: *const u8) -> &'a [u8] {
    slice::from_raw_parts(p, 6)
}
unsafe fn into_cdb10<'a>(p: *const u8) -> &'a [u8] {
    slice::from_raw_parts(p, 10)
}

fn handle_inquiry(cdb_p: *const u8, base: *mut u8, ent: &mut tcmu::tcmu_cmd_entry) -> u8 {
    let cdb = unsafe { into_cdb6(cdb_p) };
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

fn handle_mode_sense_6(cdb_p: *mut u8, ent: &mut tcmu::tcmu_cmd_entry) -> u8 {
    let cdb = unsafe { into_cdb6(cdb_p) };
    let dbd = cdb[1] & (1 << 3) > 0;
    let pc = cdb[2] >> 6;
    let page_code = cdb[2] & 0x3f;
    let subpage_code = cdb[3];
    let allocation_length = cdb[4];

    print!(
        "mode sense6: dbd {}, pc {}, page code {}, sub {}\n",
        dbd,
        pc,
        page_code,
        subpage_code
    );

    // if not supported, illegal request and invalid field in cdb.


    //NO_SENSE
    not_handled(ent);
    CHECK_CONDITION
}

const DEVICE_SIZE: usize = 1 * 1024 * 1024 * 1024;
const BLOCK_SIZE: usize = 4 * 1024;

fn htobe32(x: u32) -> u32 {
    unimplemented!()
}

fn handle_read_capacity_10(ent: &mut tcmu::tcmu_cmd_entry, base: *mut u8) -> u8 {
    let lba = htobe32((DEVICE_SIZE / BLOCK_SIZE) as u32);
    let block_size = htobe32(BLOCK_SIZE as u32);
    Responder::new(ent, base).write(unsafe {
        slice::from_raw_parts(transmute::<_, *const u8>(&lba), 4)
    });
    Responder::new(ent, base).write(unsafe {
        slice::from_raw_parts(transmute::<_, *const u8>(&block_size), 4)
    });

    NO_SENSE
}

const INQUIRY: u8 = 0x12;
const TEST_UNIT_READY: u8 = 0x00;
const MODE_SENSE_6: u8 = 0x1a;
const READ_CAPACITY_10: u8 = 0x25;
const GET_LBA_SATUS: u8 = 0x9e;
const REPORT_SUPPORTED_OPERATIONS_CODES: u8 = 0xa3;

fn do_cmd(p: *mut u8) {
    let mb: &mut tcmu::tcmu_mailbox =
        unsafe { transmute::<_, *mut tcmu::tcmu_mailbox>(p).as_mut().unwrap() };
    unsafe {
        print!(
            "mb {}, tail {}\n",
            transmute::<_, u64>(p),
            transmute::<_, u64>(&mut mb.cmd_tail)
        );
    }
    let mut ent_p = unsafe { p.offset((mb.cmdr_off + mb.cmd_tail) as isize) };
    // todo: use Atomic to load cmd_head
    print!("cmd_head {}\n", mb.cmd_head);
    print!("cmd_tail {}\n", mb.cmd_tail);
    while ent_p != unsafe { p.offset((mb.cmdr_off + mb.cmd_head) as isize) } {
        let ent = unsafe { (ent_p as *mut tcmu::tcmu_cmd_entry).as_mut().unwrap() };
        let op = tcmu::tcmu_hdr_get_op(ent.hdr.len_op);
        print!("op: {} id: {}\n", op, ent.hdr.cmd_id);
        if op == tcmu::tcmu_opcode::TCMU_OP_CMD as u32 {
            unsafe {
                let cdb_p = p.offset(ent.__bindgen_anon_1.req.as_ref().cdb_off as isize);
                let command = *cdb_p.as_ref().unwrap();
                print!("SCSI opcode: 0x{:x}\n", command);
                let status = match command {
                    INQUIRY => handle_inquiry(cdb_p, p, ent),
                    TEST_UNIT_READY => handle_test_unit_ready(),
                    MODE_SENSE_6 => handle_mode_sense_6(cdb_p, ent),
                    READ_CAPACITY_10 => handle_read_capacity_10(ent, p),
                    _ => {
                        not_handled(ent);
                        CHECK_CONDITION
                    }
                };
                ent.__bindgen_anon_1.rsp.as_mut().scsi_status = status;
            }
        } else if op == tcmu::tcmu_opcode::TCMU_OP_PAD as u32 {
            // do nothing
        } else {
            panic!("unknown cmd: {}", op);
        }
        unsafe {
            ptr::write_volatile(
                &mut mb.cmd_tail,
                (mb.cmd_tail + tcmu::tcmu_hdr_get_len(ent.hdr.len_op)) % mb.cmdr_size,
            )
        };
        print!("cmd_tail {}\n", mb.cmd_tail);
        ent_p = unsafe { p.offset((mb.cmdr_off + mb.cmd_tail) as isize) };
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

    handle(&mut uio, p);
}
