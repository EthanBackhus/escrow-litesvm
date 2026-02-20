#[cfg(test)]
mod tests {
    use {
        anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas},
        anchor_spl::{
            associated_token::{self, spl_associated_token_account},
            token::TokenAccount,
        },
        litesvm::LiteSVM,
        litesvm_token::{
            spl_token::ID as TOKEN_PROGRAM_ID, CreateAssociatedTokenAccount, CreateMint, MintTo,
        },
        solana_instruction::Instruction,
        solana_keypair::Keypair,
        solana_native_token::LAMPORTS_PER_SOL,
        solana_pubkey::Pubkey,
        solana_sdk_ids::system_program::ID as SYSTEM_PROGRAM_ID,
        solana_signer::Signer,
        solana_transaction::Transaction,
        std::path::PathBuf,
    };

    static PROGRAM_ID: Pubkey = crate::ID;

    fn setup() -> LiteSVM {
        let mut svm = LiteSVM::new();
        let so_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/deploy/anchor_escrow.so");
        let program_data = std::fs::read(so_path).expect("Failed to read program SO file");
        svm.add_program(PROGRAM_ID, &program_data);
        svm
    }

    fn get_token_balance(svm: &LiteSVM, ata: &Pubkey) -> u64 {
        TokenAccount::try_deserialize(
            &mut svm.get_account(ata).unwrap().data.as_slice()
        )
        .unwrap()
        .amount
    }

    #[test]
    fn test_escrow_full_lifecycle() {
        let mut svm = setup();

        // Fund participants
        let maker = Keypair::new();
        let taker = Keypair::new();
        svm.airdrop(&maker.pubkey(), 10 * LAMPORTS_PER_SOL).unwrap();
        svm.airdrop(&taker.pubkey(), 10 * LAMPORTS_PER_SOL).unwrap();

        // Create mints (maker controls mint_a, taker controls mint_b)
        let mint_a = CreateMint::new(&mut svm, &maker)
            .authority(&maker.pubkey())
            .decimals(6)
            .send()
            .unwrap();

        let mint_b = CreateMint::new(&mut svm, &taker)
            .authority(&taker.pubkey())
            .decimals(6)
            .send()
            .unwrap();

        // Create ATAs
        let maker_ata_a = CreateAssociatedTokenAccount::new(&mut svm, &maker, &mint_a)
            .owner(&maker.pubkey()).send().unwrap();
        let maker_ata_b = CreateAssociatedTokenAccount::new(&mut svm, &maker, &mint_b)
            .owner(&maker.pubkey()).send().unwrap();
        let taker_ata_a = CreateAssociatedTokenAccount::new(&mut svm, &taker, &mint_a)
            .owner(&taker.pubkey()).send().unwrap();
        let taker_ata_b = CreateAssociatedTokenAccount::new(&mut svm, &taker, &mint_b)
            .owner(&taker.pubkey()).send().unwrap();

        // Mint initial balances
        MintTo::new(&mut svm, &maker, &mint_a, &maker_ata_a, 1_000_000_000).send().unwrap();
        MintTo::new(&mut svm, &taker, &mint_b, &taker_ata_b, 1_000_000_000).send().unwrap();

        // Derive PDAs
        let seed: u64 = 123;
        let escrow = Pubkey::find_program_address(
            &[b"escrow", maker.pubkey().as_ref(), &seed.to_le_bytes()],
            &PROGRAM_ID,
        ).0;
        let vault = associated_token::get_associated_token_address(&escrow, &mint_a);

        let associated_token_program = spl_associated_token_account::ID;

        // Make
        let make_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: crate::accounts::Make {
                maker: maker.pubkey(),
                mint_a, mint_b,
                maker_ata_a,
                escrow, vault,
                associated_token_program,
                token_program: TOKEN_PROGRAM_ID,
                system_program: SYSTEM_PROGRAM_ID,
            }.to_account_metas(None),
            data: crate::instruction::Make { deposit: 10, seed, receive: 10 }.data(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[make_ix],
            Some(&maker.pubkey()),
            &[&maker],
            svm.latest_blockhash(),
        );
        svm.send_transaction(tx).expect("Make failed");

        // Verify escrow state
        let escrow_account = svm.get_account(&escrow).unwrap();
        let escrow_data = crate::state::Escrow::try_deserialize(
            &mut escrow_account.data.as_ref()
        ).unwrap();
        assert_eq!(escrow_data.seed, seed);
        assert_eq!(escrow_data.maker, maker.pubkey());
        assert_eq!(escrow_data.mint_a, mint_a);
        assert_eq!(escrow_data.mint_b, mint_b);
        assert_eq!(escrow_data.receive, 10);
        assert_eq!(get_token_balance(&svm, &vault), 10);

        // Take
        let take_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: crate::accounts::Take {
                taker: taker.pubkey(),
                maker: maker.pubkey(),
                mint_a, mint_b,
                taker_ata_a,
                taker_ata_b,
                maker_ata_b,
                escrow, vault,
                associated_token_program,
                token_program: TOKEN_PROGRAM_ID,
                system_program: SYSTEM_PROGRAM_ID,
            }.to_account_metas(None),
            data: crate::instruction::Take.data(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[take_ix],
            Some(&taker.pubkey()),
            &[&taker],
            svm.latest_blockhash(),
        );
        svm.send_transaction(tx).expect("Take failed");

        // Verify escrow is closed and tokens transferred
        assert!(svm.get_account(&escrow).is_none(), "Escrow should be closed after take");
        assert_eq!(get_token_balance(&svm, &taker_ata_a), 10, "Taker should have received mint_a tokens");
        assert_eq!(get_token_balance(&svm, &maker_ata_b), 10, "Maker should have received mint_b tokens");

        // Make + Refund
        MintTo::new(&mut svm, &maker, &mint_a, &maker_ata_a, 1_000_000_000).send().unwrap();

        let make_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: crate::accounts::Make {
                maker: maker.pubkey(),
                mint_a, mint_b,
                maker_ata_a,
                escrow, vault,
                associated_token_program,
                token_program: TOKEN_PROGRAM_ID,
                system_program: SYSTEM_PROGRAM_ID,
            }.to_account_metas(None),
            data: crate::instruction::Make { deposit: 100, seed, receive: 100 }.data(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[make_ix],
            Some(&maker.pubkey()),
            &[&maker],
            svm.latest_blockhash(),
        );
        svm.send_transaction(tx).expect("Second make failed");

        let refund_ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: crate::accounts::Refund {
                maker: maker.pubkey(),
                mint_a,
                maker_ata_a,
                escrow, vault,
                token_program: TOKEN_PROGRAM_ID,
                system_program: SYSTEM_PROGRAM_ID,
            }.to_account_metas(None),
            data: crate::instruction::Refund.data(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[refund_ix],
            Some(&maker.pubkey()),
            &[&maker],
            svm.latest_blockhash(),
        );
        svm.send_transaction(tx).expect("Refund failed");

        assert!(svm.get_account(&escrow).is_none(), "Escrow should be closed after refund");
        // After refund: maker had 1_000_000_000 (second mint) minus 100 deposited, plus the original
        // 1_000_000_000 minus 10 from phase 1, returned. Net = 2_000_000_000 - 10.
        assert_eq!(
            get_token_balance(&svm, &maker_ata_a),
            2_000_000_000 - 10,
            "Maker should have both mints minus the first deposit after refund"
        );
    }
}