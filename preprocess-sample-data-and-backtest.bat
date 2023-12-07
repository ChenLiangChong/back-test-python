@echo off
echo runing backtest program
conda activate stock
python sample-data-preprocess.py
python backtest.py
echo Python END.
pause