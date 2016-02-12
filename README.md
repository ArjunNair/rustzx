# rustzx
ZX Spectrum emulator which I writing in rust.   
Project is just for fun and learning architecture of CPU's  
Licensed under MIT License.

## Current progress
I writing Z80 CPU emulation part at the moment  
### instruction groups
__Implemented__  
- NOP
- INC and DEC
    - 16 bit
    - 8 bit
    - 8 bit indirect (HL), (IX/IY+d)  
    - undocumented (INC IXH/IXL/IYH/IYL)
- DJNZ  (Jump if B register is non-zero)
- JR  
    - Relative
    - Relative conditional
- DAA (Decimal Adjust)
- CPL (Complement, NOT operation)
- SCF (Set carry flag)
- CCF (Invert carry flag)

__Partialy implemented__  
- ADD    
	- [ ] 8 bit  
	- [ ] 8 bit indirect (HL), (IX/IY+d)
	- [x] 16 bit  
- EX
	- [ ] EX (SP), HL/IX/IY
	- [x] EX AF, AF'
	- [ ] EX DE, HL
	- [ ] EXX
- LD
	- [x] LD A, (BC/DE)
	- [x] LD A, (NN)
	- [x] LD BC/DE/HL/IX/IY, NN
	- [x] LD HL/IX/IY, (NN)
	- [x] LD (BC/DE), A
	- [x] LD (NN), A
	- [x] LD (NN), HL/IX/IY
    - [x] LD r[y], n ; where r - 8 bit register, n - const
    - [x] LD (HL/IX+d/IY+d), n
    - [x] LD reg1, reg2
    - [x] LD (HL/IX+d/IY+d), reg
    - [x] LD reg, (HL/IX+d/IY+d)
	- [ ] LD between A and I or R
    - [ ] LD BC/DE/SP (NN)
    - [ ] LD SP, HL/IX/IY
    - [ ] LD (NN), BC/DE/SP
- ROTATE
    - [x] RRA
    - [x] RLA
    - [x] RRCA
    - [x] RLCA
    - [ ] Other instructions of group

__Not implemented__
- A lot of other instructions :smile:  
