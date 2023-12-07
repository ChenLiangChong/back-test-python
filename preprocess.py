import pandas as pd
import os

# 提示用户输入文件夹路径
folder_name = './futures_NQ'

if not os.path.exists(folder_name):
    os.makedirs(folder_name)
    print(f'文件夹 {folder_name} is creat')
# 定义列名
day_col_names = ['timestamp', 'open', 'high', 'low', 'close', 'volume', 'open interest']
interday_col_names = ['timestamp', 'open', 'high', 'low', 'close', 'volume']

# 文件列表
file_list = [
    'NQ_1day_continuous_adjusted.txt',
    'NQ_1hour_continuous_adjusted.txt',
    'NQ_30min_continuous_adjusted.txt',
    'NQ_5min_continuous_adjusted.txt',
    'NQ_1min_continuous_adjusted.txt'
]

for file_name in file_list:
    if '1day' in file_name:
        col_names = day_col_names
    else:
        col_names = interday_col_names

    # 读取数据文件
    data = pd.read_csv(f'{folder_name}/{file_name}', names=col_names)
    
    # 构建保存文件名，根据时间间隔命名
    time_interval = file_name.split('_')[1].replace('continuous', '').replace('adjusted', '').replace('.csv', '').replace('.txt', '')
    save_file_name = f'{folder_name}/NQ_{time_interval}_sample.csv'
    
    # 将数据保存为新的CSV文件
    data.to_csv(save_file_name, index=False)
    print('convert '+save_file_name+' success')

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