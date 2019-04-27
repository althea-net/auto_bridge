use clarity::abi::derive_signature;
use clarity::abi::{encode_call, Token};
use clarity::utils::bytes_to_hex_str;
use clarity::Transaction;
use clarity::{Address, PrivateKey};
use failure::{ensure, Error};
use futures::{Future, IntoFuture};
use futures_timer::Delay;
use num256::Uint256;
use rand::prelude::*;
use std::time::Duration;
use web30::client::Web3;
use web30::types::Log;

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

pub fn address_to_bytes_32(address: Address) -> [u8; 32] {
    let mut data: [u8; 32] = Default::default();
    data[12..].copy_from_slice(&address.as_bytes());
    data
}

pub struct TokenBridge {
    xdai_web3: Web3,
    eth_web3: Web3,
    uniswap_address: Address,
    /// This is the address of the xDai bridge on Eth
    xdai_foreign_bridge_address: Address,
    /// This is the address of the xDai bridge on xDai
    xdai_home_bridge_address: Address,
    /// This is the address of the Dai token contract on Eth
    foreign_dai_contract_address: Address,
    own_address: Address,
    secret: PrivateKey,
}

impl TokenBridge {
    pub fn new(
        uniswap_address: Address,
        xdai_home_bridge_address: Address,
        xdai_foreign_bridge_address: Address,
        foreign_dai_contract_address: Address,
        own_address: Address,
        secret: PrivateKey,
        eth_full_node_url: &String,
        xdai_full_node_url: &String,
    ) -> TokenBridge {
        TokenBridge {
            uniswap_address,
            xdai_home_bridge_address,
            xdai_foreign_bridge_address,
            foreign_dai_contract_address,
            own_address,
            secret,
            xdai_web3: Web3::new(xdai_full_node_url),
            eth_web3: Web3::new(eth_full_node_url),
        }
    }

    /// Price of ETH in Dai
    pub fn eth_to_dai_price(&self, amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let web3 = self.eth_web3.clone();
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
        let web3 = self.eth_web3.clone();
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
        let web3 = self.eth_web3.clone();

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
            None,
            |_| true,
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

    /// Sell `dai_amount` Dai for ETH
    pub fn dai_to_eth_swap(
        &self,
        dai_amount: Uint256,
    ) -> Box<Future<Item = Uint256, Error = Error>> {
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();
        let web3 = self.eth_web3.clone();

        let event = web3.wait_for_event(
            uniswap_address,
            "TokenPurchase(address,uint256,uint256)",
            Some(vec![own_address.into()]),
            None,
            None,
            |_| true,
        );

        Box::new(
            web3.eth_block_number()
                .and_then({
                    let web3 = web3.clone();
                    move |current_block| web3.eth_get_block_by_number(current_block)
                })
                .join(self.dai_to_eth_price(dai_amount.clone()))
                .and_then(move |(block, expected_eth)| {
                    // Equivalent to `amount * (1 - 0.025)` without using decimals
                    let expected_eth = (expected_eth / 40u64.into()) * 39u64.into();
                    let payload = encode_call(
                        "tokenToEthSwapInput(uint256,uint256,uint256)",
                        &[
                            dai_amount.into(),
                            expected_eth.into(),
                            block.timestamp.into(),
                        ],
                    );

                    let call = web3.send_transaction(
                        uniswap_address,
                        payload,
                        0u32.into(),
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

    /// Convert `dai_amount` dai to xdai
    pub fn dai_to_xdai_bridge(
        &self,
        dai_amount: Uint256,
    ) -> Box<Future<Item = Log, Error = Error>> {
        let xdai_web3 = self.xdai_web3.clone();
        let eth_web3 = self.eth_web3.clone();
        let xdai_home_bridge_address = self.xdai_home_bridge_address.clone();
        let xdai_foreign_bridge_address = self.xdai_home_bridge_address.clone();
        let foreign_dai_contract_address = self.foreign_dai_contract_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();

        // We tag the amount with a small random number so that we can identify it later
        let tagged_amount: Uint256 = dai_amount + rand::random::<u16>().into();

        let payload = encode_call(
            "transfer(address,uint256)",
            &[
                xdai_foreign_bridge_address.into(),
                tagged_amount.clone().into(),
            ],
        );

        // You basically just send it some coins
        Box::new(
            xdai_web3
                .send_transaction(
                    xdai_home_bridge_address,
                    payload,
                    0u32.into(),
                    own_address,
                    secret,
                )
                .and_then(move |_| {
                    eth_web3.wait_for_event(
                        foreign_dai_contract_address,
                        "Transfer(address,address,uint256)",
                        Some(vec![xdai_foreign_bridge_address.into()]),
                        Some(vec![own_address.into()]),
                        None,
                        move |log| Uint256::from_bytes_be(&log.data) == tagged_amount,
                    )
                })
                .and_then(|log| futures::future::ok(log)),
        )
    }

    // var ethers = require("ethers")

    // var parm = ethers.utils.defaultAbiCoder.decode(["uint256"], "0x00000000000000000000000000000000000000000000000001ade0dbe1d28000").toString();

    // console.log(parm)

    /// Convert `xdai_amount` xdai to dai
    /// As part of the conversion the amount to be sent is "tagged" by adding a tiny amount to it,
    /// up to 65 kwei
    pub fn xdai_to_dai_bridge(
        &self,
        xdai_amount: Uint256,
    ) -> Box<Future<Item = Log, Error = Error>> {
        let xdai_web3 = self.xdai_web3.clone();
        let eth_web3 = self.eth_web3.clone();
        let xdai_home_bridge_address = self.xdai_home_bridge_address.clone();
        let xdai_foreign_bridge_address = self.xdai_home_bridge_address.clone();
        let foreign_dai_contract_address = self.foreign_dai_contract_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();

        // We tag the amount with a small random number so that we can identify it later
        let tagged_amount: Uint256 = xdai_amount + rand::random::<u16>().into();

        // You basically just send it some coins
        Box::new(
            xdai_web3
                .send_transaction(
                    xdai_home_bridge_address,
                    [].to_vec(),
                    tagged_amount.clone(),
                    own_address,
                    secret,
                )
                .join(eth_web3.wait_for_event(
                    foreign_dai_contract_address,
                    "Transfer(address,address,uint256)",
                    Some(vec![xdai_foreign_bridge_address.into()]),
                    Some(vec![own_address.into()]),
                    None,
                    move |log| Uint256::from_bytes_be(&log.data) == tagged_amount,
                ))
                .and_then(|(_, log)| futures::future::ok(log)),
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
