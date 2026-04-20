use futures_core::stream::Stream;

pub fn create_stream() -> impl Stream<Item = u8> {
    reexporter::stream! {
        for x in 0..10_u8 {
            yield x;
        }
    }
}

pub fn create_try_stream() -> impl Stream<Item = Result<u8, u8>> {
    reexporter::try_stream! {
        for x in 0..10_u8 {
            yield x;
        }
    }
}
