use std::num::NonZeroUsize;
use std::thread;

use gil::spsc::parking::channel;

fn main() {
    let size = NonZeroUsize::new(512).unwrap();
    for attempt in 0..1000 {
        let (mut tx1, mut rx1) = channel::<u8>(size);
        let (mut tx2, mut rx2) = channel::<u8>(size);

        let t = thread::spawn(move || {
            for i in 0..100 {
                let _x = rx1.recv();
                tx2.send(i as u8);
            }
        });

        for i in 0..100 {
            tx1.send(i as u8);
            let _x = rx2.recv();
        }
        t.join().unwrap();
        if attempt % 100 == 0 {
            eprintln!("attempt {attempt} ok");
        }
    }
    eprintln!("all done");
}
