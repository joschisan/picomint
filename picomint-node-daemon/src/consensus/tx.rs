use picomint_core::Amount;
use picomint_core::tx::TxError;

#[derive(Clone, Debug, Default)]
pub struct FundingVerifier {
    inputs: Amount,
    outputs: Amount,
    fees: Amount,
}

impl FundingVerifier {
    /// The amount funds the transaction while the fee consumes funding.
    pub fn add_input(&mut self, amount: Amount, fee: Amount) -> Result<&mut Self, TxError> {
        self.inputs = self.inputs.checked_add(amount).ok_or(TxError::Overflow)?;

        self.fees = self.fees.checked_add(fee).ok_or(TxError::Overflow)?;

        Ok(self)
    }

    /// Both the amount and the fee consume funding.
    pub fn add_output(&mut self, amount: Amount, fee: Amount) -> Result<&mut Self, TxError> {
        self.outputs = self.outputs.checked_add(amount).ok_or(TxError::Overflow)?;

        self.fees = self.fees.checked_add(fee).ok_or(TxError::Overflow)?;

        Ok(self)
    }

    pub fn verify_funding(self) -> Result<(), TxError> {
        let outputs_and_fees = self
            .outputs
            .checked_add(self.fees)
            .ok_or(TxError::Overflow)?;

        if self.inputs >= outputs_and_fees {
            return Ok(());
        }

        Err(TxError::Underfunded)
    }
}

#[cfg(test)]
mod tests {
    use picomint_core::Amount;

    #[test]
    fn sanity_test_funding_verifier() {
        let mut v = super::FundingVerifier::default();

        v.add_input(Amount::from_msat(3), Amount::from_msat(1))
            .unwrap()
            .add_output(Amount::from_msat(1), Amount::from_msat(1))
            .unwrap();

        assert!(v.clone().verify_funding().is_ok());

        v.add_output(Amount::from_msat(1), Amount::ZERO).unwrap();

        assert!(v.clone().verify_funding().is_err());

        v.add_input(Amount::from_msat(10), Amount::ZERO).unwrap();

        // Overfunding is always allowed
        assert!(v.clone().verify_funding().is_ok());
    }
}
