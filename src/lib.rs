use clarity::abi::derive_signature;
use clarity::abi::{encode_call, Token};
use clarity::utils::bytes_to_hex_str;
use clarity::Transaction;
use clarity::{Address, PrivateKey};
use failure::{ensure, Error};
use futures::{Future, IntoFuture};
use num256::Uint256;
use web30::client::Web3;

// const ALLOWED_SLIPPAGE = 0.025;
// const TOKEN_ALLOWED_SLIPPAGE = 0.04;

// // Sell ETH for ERC20
// inputAmount = userInputEthValue
// input_reserve = web3.eth.getBalance(exchangeAddress)
// output_reserve = tokenContract.methods.balanceOf(exchangeAddress)

// // Sell ERC20 for ETH
// inputAmount = userInputTokenValue
// input_reserve = tokenContract.methods.balanceOf(exchangeAddress)
// output_reserve = web3.eth.getBalance(exchangeAddress)

// // Output amount bought
// numerator = inputAmount * output_reserve * 997
// denominator = input_reserve * 1000 + inputAmount * 997
// outputAmount = numerator / denominator

pub struct TokenBridge {
    web3: Web3,
    uniswap_address: Address,
    xdai_bridge_address: Address,
    own_address: Address,
    secret: PrivateKey,
}

impl TokenBridge {
    pub fn new(
        uniswap_address: Address,
        xdai_bridge_address: Address,
        own_address: Address,
        secret: PrivateKey,
        full_node_url: &String,
    ) -> TokenBridge {
        TokenBridge {
            uniswap_address,
            xdai_bridge_address,
            own_address,
            secret,
            web3: Web3::new(full_node_url),
        }
    }

    /// Price of ETH in Dai
    pub fn eth_to_dai_price(&self, amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let web3 = self.web3.clone();
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();

        let props = web3
            .eth_get_balance(uniswap_address)
            .join(web3.contract_call(
                uniswap_address,
                "balanceOf(address)",
                &[own_address.into()],
                own_address,
            ));

        Box::new(props.and_then(move |(input_reserve, output_reserve)| {
            let numerator = amount.clone() * output_reserve * 997u64.into();
            let denominator = input_reserve * 1000u64.into() + amount * 997u64.into();
            Ok(numerator / denominator)
        }))
    }

    /// Price of Dai in ETH
    pub fn dai_to_eth_price(&self, amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let web3 = self.web3.clone();
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();

        let props = web3
            .eth_get_balance(uniswap_address)
            .join(web3.contract_call(
                uniswap_address,
                "balanceOf(address)",
                &[own_address.into()],
                own_address,
            ));

        Box::new(props.and_then(move |(output_reserve, input_reserve)| {
            let numerator = amount.clone() * output_reserve * 997u64.into();
            let denominator = input_reserve * 1000u64.into() + amount * 997u64.into();
            Ok(numerator / denominator)
        }))
    }

    /// Sell `eth_amount` ETH for Dai
    pub fn eth_to_dai_swap(
        &self,
        eth_amount: Uint256,
    ) -> Box<Future<Item = Uint256, Error = Error>> {
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();
        let web3 = self.web3.clone();

        let own_address_bytes: [u8; 32] = {
            let mut data: [u8; 32] = Default::default();
            data[12..].copy_from_slice(&own_address.as_bytes());
            data
        };

        let event = web3.wait_for_event(
            uniswap_address,
            "TokenPurchase(address,uint256,uint256)",
            Some(vec![own_address_bytes]),
            None,
        );

        Box::new(
            web3.eth_block_number()
                .and_then({
                    let web3 = web3.clone();
                    move |current_block| web3.eth_get_block_by_number(current_block)
                })
                .join(self.eth_to_dai_price(eth_amount.clone()))
                .and_then(move |(block, expected_dai)| {
                    // Equivalent to `amount * (1 - 0.025)` without using decimals
                    let expected_dai = (expected_dai / 40u64.into()) * 39u64.into();
                    let payload = encode_call(
                        "ethToTokenSwapInput(uint256,uint256)",
                        &[expected_dai.into(), block.timestamp.into()],
                    );

                    let call = web3.send_transaction(
                        uniswap_address,
                        payload,
                        eth_amount,
                        own_address,
                        secret,
                    );

                    Box::new(
                        call.join(event)
                            .and_then(|(_tx, response)| {
                                let mut data: [u8; 32] = Default::default();
                                data.copy_from_slice(&response.data);
                                Ok(data.into())
                            })
                            .into_future(),
                    )
                }),
        )
    }

    /// Sell `eth_amount` Dai for ETH
    pub fn dai_to_eth_swap(
        &self,
        dai_amount: Uint256,
    ) -> Box<Future<Item = Uint256, Error = Error>> {
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();
        let web3 = self.web3.clone();

        let own_address_bytes: [u8; 32] = {
            let mut data: [u8; 32] = Default::default();
            data[12..].copy_from_slice(&own_address.as_bytes());
            data
        };

        let event = web3.wait_for_event(
            uniswap_address,
            "TokenPurchase(address,uint256,uint256)",
            Some(vec![own_address_bytes]),
            None,
        );

        Box::new(
            self.dai_to_eth_price(dai_amount.clone())
                .and_then(move |expected_eth| {
                    // Equivalent to `amount * (1 - 0.025)` without using decimals
                    let expected_eth = (expected_eth / 40u64.into()) * 39u64.into();
                    let payload = encode_call(
                        "tokenToEthSwapInput(uint256,uint256)",
                        &[expected_eth.into()],
                    );

                    let call = web3.send_transaction(
                        uniswap_address,
                        payload,
                        dai_amount,
                        own_address,
                        secret,
                    );

                    Box::new(
                        call.join(event)
                            .and_then(|(_tx, response)| {
                                let mut data: [u8; 32] = Default::default();
                                data.copy_from_slice(&response.data);
                                Ok(data.into())
                            })
                            .into_future(),
                    )
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
