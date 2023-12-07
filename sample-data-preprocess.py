import pandas as pd
import os
# 提示用户输入文件夹路径
folder_name = './futures_NQ'

if not os.path.exists(folder_name):
    os.makedirs(folder_name)
    print(f'文件夹 {folder_name} is creat')

# 创建一个15分钟间隔的CSV文件
data = pd.read_csv(f'{folder_name}/NQ_1min_sample.csv', parse_dates=['timestamp'])
data.set_index('timestamp', inplace=True)

ohlc_15min = data.resample('15T').agg({
    'open': 'first',
    'high': 'max',
    'low': 'min',
    'close': 'last',
    'volume': 'sum',
})

ohlc_15min = ohlc_15min.dropna()

# 保存15分钟间隔的CSV文件
ohlc_15min.to_csv(f'{folder_name}/NQ_15min_sample.csv')