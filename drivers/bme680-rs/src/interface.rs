use embedded_hal_async::{
    i2c::I2c,
};

#[allow(async_fn_in_trait)]
pub(crate) trait Interface {
    type Error;

    /// Read multiple bytes starting from the given register address into the provided buffer.
    async fn read_registers(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), Self::Error>;

    /// Write multiple bytes starting from the given register address from the provided buffer.
    async fn write_registers(&mut self, reg: u8, data: &[u8]) -> Result<(), Self::Error>;
}

/// I2C interface for the BME680 sensor.
pub(crate) struct I2cInterface<I2C> {
    pub(crate) i2c: I2C,
    pub(crate) address: u8,
}

impl<I2C, E> Interface for I2cInterface<I2C>
where
    I2C: I2c<Error = E>,
{
    type Error = E;

    async fn read_registers(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.i2c.write_read(self.address, &[reg], buf).await
    }

    async fn write_registers(&mut self, reg: u8, data: &[u8]) -> Result<(), Self::Error> {
        for (i, d) in data.iter().enumerate() {
            self.i2c.write(self.address, &[reg + i as u8, *d]).await?;
        }
        Ok(())
    }
}

pub(crate) trait ReadPrimitive<E> {
    async fn read_primitive<IFACE: Interface<Error = E>>(iface: &mut IFACE, reg: u8) -> Result<Self, E> where Self: Sized;
}

macro_rules! impl_read_primitive {
    ($ty:ty) => {
        impl<E> ReadPrimitive<E> for $ty {
            async fn read_primitive<IFACE: Interface<Error = E>>(iface: &mut IFACE, reg: u8) -> Result<Self, E> {
                let mut buf = [0u8; core::mem::size_of::<$ty>()];
                iface.read_registers(reg, &mut buf).await?;
                Ok(Self::from_le_bytes(buf.try_into().unwrap()))
            }
        }
    };
}

impl_read_primitive!(i8);
impl_read_primitive!(u8);
impl_read_primitive!(i16);
impl_read_primitive!(u16);
