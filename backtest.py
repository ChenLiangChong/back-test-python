import pandas as pd
from lightweight_charts import Chart
import asyncio
from datetime import timedelta
import atexit
import numpy as np
import tkinter as tk
from tkcalendar import Calendar
from datetime import datetime

class TradeRecord:
    def __init__(self):
        self.trades = pd.DataFrame(columns=['timestamp', 'action', 'price', 'spread'])
        self.trades['price'] = self.trades['price'].astype(float)
        self.trades['spread'] = self.trades['spread'].astype(float)
        self.current_action = None  # 當前交易動作
        self.last_price = None  # 上一次交易的價格

    def add_trade(self, timestamp, action, price):
        if len(self.trades) % 2 == 1 and action == self.current_action:
            raise ValueError("Cannot perform the same type of trade consecutively.")

        if len(self.trades) % 2 == 1:
            spread = self.last_price - price if self.current_action == 'Sell' else price - self.last_price
            new_trade = pd.DataFrame({'timestamp': [timestamp], 'action': [action], 'price': [price], 'spread': [spread]})
        else:
            new_trade = pd.DataFrame({'timestamp': [timestamp], 'action': [action], 'price': [price], 'spread': [np.nan]})
        
        if len(self.trades) == 0:
            self.trades = new_trade
        else:
            self.trades = pd.concat([self.trades, new_trade], ignore_index=True)
        self.current_action = action
        self.last_price = price

    def get_trades(self):
        return self.trades
    
# 在程序退出时保存trade_record为CSV文件
def save_trade_record_to_csv():
    trade_record_df = trade_record.get_trades()
    if not trade_record_df.empty:
        trade_record_df.to_csv('trade_record.csv', index=False)

def round_down_to_nearest(timestamp, minutes_rounding):
    floor_minutes = (timestamp.minute // minutes_rounding) * minutes_rounding
    rounded_time = timestamp.replace(minute=floor_minutes, second=0, microsecond=0)
    return rounded_time

atexit.register(save_trade_record_to_csv)

# 初始化pause变量，初始值为False
pause = True
paused_index = 0  # 记录暂停时的索引
playSpeed = 1
trade_record = TradeRecord()

oneMinDf = pd.read_csv('./futures_NQ/NQ_1min_sample.csv', parse_dates=['timestamp'])
oneMinDf = oneMinDf.rename(columns={'timestamp': 'time'})

min1MTime = oneMinDf[:1]['time'][0]

minTime = oneMinDf.iloc[0]['time']  # 获取第一行
maxTime = oneMinDf.iloc[-1]['time']  # 获取最后一行

fiveMinDf = pd.read_csv('./futures_NQ/NQ_5min_sample.csv', parse_dates=['timestamp'])
fiveMinDf = fiveMinDf.rename(columns={'timestamp': 'time'})
min5MTime = fiveMinDf[:1]['time'][0]

fifteenMinDf = pd.read_csv('./futures_NQ/NQ_15min_sample.csv', parse_dates=['timestamp'])
fifteenMinDf = fifteenMinDf.rename(columns={'timestamp': 'time'})
min15MTime = fifteenMinDf[:1]['time'][0]

thirtyMinDf = pd.read_csv('./futures_NQ/NQ_30min_sample.csv', parse_dates=['timestamp'])
thirtyMinDf = thirtyMinDf.rename(columns={'timestamp': 'time'})
min30MTime = thirtyMinDf[:1]['time'][0]

oneHourDf = pd.read_csv('./futures_NQ/NQ_1hour_sample.csv', parse_dates=['timestamp'])
oneHourDf = oneHourDf.rename(columns={'timestamp': 'time'})
min1HTime = oneHourDf[:1]['time'][0]

df1 = oneMinDf[:1]
updateDf = oneMinDf[1:]


async def data_loop(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime,playSpeed  # 声明要修改全局变量的意图
    row_index = updateDf.index[0]
    paused_index = 0
    while True:
        if not chart.is_alive:
            break
        timeframe = chart.topbar["timeframe"].value

        if pause:
            await asyncio.sleep(playSpeed)
            continue  # 如果暂停，不执行下面的代码
        try:
            if timeframe == '1min':
                min1MTime = updateDf.iloc[paused_index+1]['time']
            elif timeframe == '5min':
                min5MTime = updateDf.iloc[paused_index+1]['time']
            elif timeframe == '15min':
                min15MTime = updateDf.iloc[paused_index+1]['time']
            elif timeframe == '30min':
                min30MTime = updateDf.iloc[paused_index+1]['time']
            elif timeframe == '1hour':
                min1HTime = updateDf.iloc[paused_index+1]['time']

            chart.update(updateDf.iloc[paused_index])
            paused_index += 1

            await asyncio.sleep(playSpeed)
        except:
            pause = True
            print("End of data")

def on_button_press(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime  # 声明要修改全局变量的意图
    new_button_value = 'On' if chart.topbar['my_button'].value == 'Off' else 'Off'
    chart.topbar['my_button'].set(new_button_value)
    # 切换暂停状态
    if new_button_value == 'On':
        pause = True
    else:
        pause = False
        global paused_index
        # 从上次暂停的索引开始继续执行
        chart.update(updateDf.iloc[paused_index])

def on_button_long(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime  # 声明要修改全局变量的意图

    new_button_value = 'On'
    chart.topbar['my_button'].set('On')
    # 切换暂停状态
    if new_button_value == 'On':
        pause = True  # 声明要修改全局变量的意图

    timeframe = chart.topbar["timeframe"].value
    tempTime = updateDf.iloc[paused_index]['time']
    try:
        if timeframe == '1min':
            oneMinIndex = oneMinDf[oneMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(oneMinDf.iloc[oneMinIndex]['time'],'Buy',oneMinDf.iloc[oneMinIndex]['close'])
        elif timeframe == '5min':
            fiveMinIndex = fiveMinDf[fiveMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(fiveMinDf.iloc[fiveMinIndex]['time'],'Buy',fiveMinDf.iloc[fiveMinIndex]['close'])
        elif timeframe == '15min':
            fifteenMinIndex = fifteenMinDf[fifteenMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(fifteenMinDf.iloc[fifteenMinIndex]['time'],'Buy',fifteenMinDf.iloc[fifteenMinIndex]['close'])
        elif timeframe == '30min':
            thirtyMinIndex = thirtyMinDf[thirtyMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(thirtyMinDf.iloc[thirtyMinIndex]['time'],'Buy',thirtyMinDf.iloc[thirtyMinIndex]['close'])
        elif timeframe == '1hour':
            oneHourIndex = oneHourDf[oneHourDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(oneHourDf.iloc[oneHourIndex]['time'],'Buy',oneHourDf.iloc[oneHourIndex]['close'])

        chart.marker(text='long')
    except Exception as e:
        print(e)
        pass

def on_button_short(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime  # 声明要修改全局变量的意图

    new_button_value = 'On'
    chart.topbar['my_button'].set('On')
    # 切换暂停状态
    if new_button_value == 'On':
        pause = True  # 声明要修改全局变量的意图

    timeframe = chart.topbar["timeframe"].value
    tempTime = updateDf.iloc[paused_index]['time']
    try:
        if timeframe == '1min':
            oneMinIndex = oneMinDf[oneMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(oneMinDf.iloc[oneMinIndex]['time'],'Sell',oneMinDf.iloc[oneMinIndex]['close'])
        elif timeframe == '5min':
            fiveMinIndex = fiveMinDf[fiveMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(fiveMinDf.iloc[fiveMinIndex]['time'],'Sell',fiveMinDf.iloc[fiveMinIndex]['close'])
        elif timeframe == '15min':
            fifteenMinIndex = fifteenMinDf[fifteenMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(fifteenMinDf.iloc[fifteenMinIndex]['time'],'Sell',fifteenMinDf.iloc[fifteenMinIndex]['close'])
        elif timeframe == '30min':
            thirtyMinIndex = thirtyMinDf[thirtyMinDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(thirtyMinDf.iloc[thirtyMinIndex]['time'],'Sell',thirtyMinDf.iloc[thirtyMinIndex]['close'])
        elif timeframe == '1hour':
            oneHourIndex = oneHourDf[oneHourDf['time'] == tempTime].index[0]-1
            trade_record.add_trade(oneHourDf.iloc[oneHourIndex]['time'],'Sell',oneHourDf.iloc[oneHourIndex]['close'])

        chart.marker(text='short')
    except Exception as e:
        print(e)
        pass
    

def on_next_candle(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime

    new_button_value = 'On'
    chart.topbar['my_button'].set('On')
    # 切换暂停状态
    if new_button_value == 'On':
        pause = True  # 声明要修改全局变量的意图

    timeframe = chart.topbar["timeframe"].value
    if timeframe == '1min':
        min1MTime = updateDf.iloc[paused_index+1]['time']
    elif timeframe == '5min':
        min5MTime = updateDf.iloc[paused_index+1]['time']
    elif timeframe == '15min':
        min15MTime = updateDf.iloc[paused_index+1]['time']
    elif timeframe == '30min':
        min30MTime = updateDf.iloc[paused_index+1]['time']
    elif timeframe == '1hour':
        min1HTime = updateDf.iloc[paused_index+1]['time']

    chart.update(updateDf.iloc[paused_index])
    paused_index += 1

def on_pervious_candle(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime

    new_button_value = 'On'
    chart.topbar['my_button'].set('On')
    # 切换暂停状态
    if new_button_value == 'On':
        pause = True  # 声明要修改全局变量的意图

    timeframe = chart.topbar["timeframe"].value
    tempTime = updateDf.iloc[paused_index]['time']

    if timeframe == '1min':

        oneMinIndex = oneMinDf[oneMinDf['time'] == tempTime].index[0]-1
        chart.set(oneMinDf[:oneMinIndex], render_drawings=True)
        min1MTime = tempTime
        paused_index = 0
        updateDf = oneMinDf[oneMinIndex:]
        
    elif timeframe == '5min':
        fiveMinIndex = fiveMinDf[fiveMinDf['time'] == tempTime].index[0]-1
        chart.set(fiveMinDf[:fiveMinIndex], render_drawings=True)
        min5MTime = tempTime
        paused_index = 0
        updateDf = fiveMinDf[fiveMinIndex:]

    elif timeframe == '15min':
        fifteenMinIndex = fifteenMinDf[fifteenMinDf['time'] == tempTime].index[0]-1
        chart.set(fifteenMinDf[:fifteenMinIndex], render_drawings=True)
        min15MTime = tempTime
        paused_index = 0
        updateDf = fifteenMinDf[fifteenMinIndex:]

    elif timeframe == '30min':
        thirtyMinIndex = thirtyMinDf[thirtyMinDf['time'] == tempTime].index[0]-1
        chart.set(thirtyMinDf[:thirtyMinIndex], render_drawings=True)
        min30MTime = tempTime
        paused_index = 0
        updateDf = thirtyMinDf[thirtyMinIndex:]

    elif timeframe == '1hour':
        oneHourIndex = oneHourDf[oneHourDf['time'] == tempTime].index[0]-1
        chart.set(oneHourDf[:oneHourIndex], render_drawings=True)
        min1HTime = tempTime
        paused_index = 0
        updateDf = oneHourDf[oneHourIndex:]

def on_speed_selection(chart):
    global playSpeed
    mPlaySpeed = chart.topbar["playSpeed"].value
    if mPlaySpeed == '1sec':
        playSpeed = 1
    elif mPlaySpeed == '0.5sec':
        playSpeed = 0.5
    elif mPlaySpeed == '0.25sec':
        playSpeed = 0.25
    elif mPlaySpeed == '0.1sec':
        playSpeed = 0.1
    elif mPlaySpeed == '0.05sec':
        playSpeed = 0.05
    elif mPlaySpeed == '0.01sec':
        playSpeed = 0.01


def on_timeframe_selection(chart):
    global pause, paused_index,updateDf,min1MTime,min5MTime,min15MTime,min30MTime,min1HTime,earlyTimestamp

    new_button_value = 'On'
    chart.topbar['my_button'].set('On')
    # 切换暂停状态
    if new_button_value == 'On':
        pause = True

    tempTime = updateDf.iloc[paused_index]['time']
    rounded_1min = round_down_to_nearest(tempTime, 1)
    rounded_5min = round_down_to_nearest(tempTime, 5)
    rounded_15min = round_down_to_nearest(tempTime, 15)
    rounded_30min = round_down_to_nearest(tempTime, 30)
    rounded_1hour = round_down_to_nearest(tempTime, 60)
    print(tempTime,rounded_1min,rounded_5min,rounded_15min,rounded_30min,rounded_1hour)
    timeframe = chart.topbar["timeframe"].value
    if timeframe == '1min':
        print(min1MTime,rounded_1min)
        if min1MTime > rounded_1min and min1MTime - rounded_1min < timedelta(minutes=5):
            oneMinIndex = oneMinDf[oneMinDf['time'] == min1MTime].index[0]
        else:
            oneMinIndex = oneMinDf[oneMinDf['time'] == rounded_1min].index[0]
            min1MTime = rounded_1min

        chart.set(oneMinDf[:oneMinIndex], render_drawings=True)
        paused_index = 0
        updateDf = oneMinDf[oneMinIndex:]
        print("select 1m")

    elif timeframe == '5min':
        print(min5MTime,rounded_5min)
        if min5MTime > rounded_5min and min5MTime - rounded_5min < timedelta(minutes=15):
            fiveMinIndex = fiveMinDf[fiveMinDf['time'] == min5MTime].index[0]
        else:
            fiveMinIndex = fiveMinDf[fiveMinDf['time'] == rounded_5min].index[0]
            min5MTime = rounded_5min

        chart.set(fiveMinDf[:fiveMinIndex], render_drawings=True)
        paused_index = 0
        updateDf = fiveMinDf[fiveMinIndex:]
        # rounded_5min
        print("select 5m")

    elif timeframe == '15min':
        print(min15MTime,rounded_15min)
        if min15MTime > rounded_15min and min15MTime - rounded_15min < timedelta(minutes=30):
            fifteenMinIndex = fifteenMinDf[fifteenMinDf['time'] == min15MTime].index[0]
        else:
            fifteenMinIndex = fifteenMinDf[fifteenMinDf['time'] == rounded_15min].index[0]
            min15MTime = rounded_1min

        chart.set(fifteenMinDf[:fifteenMinIndex], render_drawings=True)
        paused_index = 0
        updateDf = fifteenMinDf[fifteenMinIndex:]
        print("select 15m")

    elif timeframe == '30min':
        print(min30MTime,rounded_30min)
        if min30MTime > rounded_30min and min30MTime - rounded_30min < timedelta(minutes=60):
            thirtyMinIndex = thirtyMinDf[thirtyMinDf['time'] == min30MTime].index[0]
        else:
            thirtyMinIndex = thirtyMinDf[thirtyMinDf['time'] == rounded_30min].index[0]
            min30MTime = rounded_1min

        chart.set(thirtyMinDf[:thirtyMinIndex], render_drawings=True)
        paused_index = 0
        updateDf = thirtyMinDf[thirtyMinIndex:]
        print("select 30m")

    elif timeframe == '1hour':
        print(min1HTime,rounded_1hour)
        if min1HTime > rounded_1hour:
            oneHourIndex = oneHourDf[oneHourDf['time'] == min1HTime].index[0]
        else:
            oneHourIndex = oneHourDf[oneHourDf['time'] == rounded_1hour].index[0]
            min1HTime = rounded_1hour

        chart.set(oneHourDf[:oneHourIndex], render_drawings=True)
        paused_index = 0
        updateDf = oneHourDf[oneHourIndex:]
        print("select 1h")

    print(f'Selected timeframe of {chart.topbar["timeframe"].value}')

async def main():
    global df1

    chart = Chart(toolbox=True)

    chart.topbar.button('my_button', 'Off', func=on_button_press)

    chart.topbar.button('pervious_candle', '<<', func=on_pervious_candle)

    chart.topbar.button('next_candle', '>>', func=on_next_candle)

    chart.topbar.button('long', 'long', func=on_button_long)

    chart.topbar.button('short', 'short', func=on_button_short)

    chart.topbar.switcher('timeframe', ('1min', '5min', '15min', '30min','1hour'), func=on_timeframe_selection)

    chart.topbar.switcher('playSpeed', ('1sec', '0.5sec', '0.25sec', '0.1sec','0.05sec','0.01sec'), func=on_speed_selection)
    
    chart.set(df1)
    await asyncio.gather(chart.show_async(block=True), data_loop(chart))

# 创建一个函数来处理用户的选择
def start_program():
    global timestamp,updateDf,df1,earlyTimestamp,early_1min,early_5min,early_15min,early_30min,early_1hour,oneMinDf,fiveMinDf,fifteenMinDf,thirtyMinDf,oneHourDf
    # 销毁窗口
    selected_date = calendar.get_date()
    selected_hour = hour_var.get()
    selected_min = minute_var.get()

    selected_datetime_str = f"{selected_date} {selected_hour} {selected_min}"
    print(selected_datetime_str)

    try:
    # 将字符串转换为datetime对象
        selected_datetime = datetime.strptime(selected_datetime_str, "%Y-%m-%d %H %M")
        # 将datetime对象转换为Timestamp格式
        timestamp = pd.Timestamp(selected_datetime)
        print("選擇的時間：", timestamp)
        oneMinIndex = oneMinDf[oneMinDf['time'] == timestamp].index[0]



        if oneMinIndex - 25000>0:
            earlyTimestamp = oneMinDf.loc[oneMinIndex - 25000]['time']
            earlyOneMinIndex = oneMinDf[oneMinDf['time'] == earlyTimestamp].index[0]

            early_5min = round_down_to_nearest(earlyTimestamp, 5)
            earlyFiveMinIndex = fiveMinDf[fiveMinDf['time'] == early_5min].index[0]


            early_15min = round_down_to_nearest(earlyTimestamp, 15)
            earlyFifteenMinIndex = fifteenMinDf[fifteenMinDf['time'] == early_15min].index[0]

            early_30min = round_down_to_nearest(earlyTimestamp, 30)
            earlyThirtyMinIndex = thirtyMinDf[thirtyMinDf['time'] == early_30min].index[0]


            early_1hour = round_down_to_nearest(earlyTimestamp, 60)
            earlyOneHourIndex = oneHourDf[oneHourDf['time'] == early_1hour].index[0]

            oneMinDf = oneMinDf[earlyOneMinIndex:].reset_index(drop=True)
            fiveMinDf = fiveMinDf[earlyFiveMinIndex:].reset_index(drop=True)
            fifteenMinDf = fifteenMinDf[earlyFifteenMinIndex:].reset_index(drop=True)
            thirtyMinDf = thirtyMinDf[earlyThirtyMinIndex:].reset_index(drop=True)
            oneHourDf = oneHourDf[earlyOneHourIndex:].reset_index(drop=True)

            oneMinIndex = oneMinDf[oneMinDf['time'] == timestamp].index[0]

            df1=oneMinDf[:oneMinIndex]
            updateDf = oneMinDf[oneMinIndex:]
        else:
            earlyTimestamp = oneMinDf.loc[0]['time']
            df1 = oneMinDf[:oneMinIndex]
            updateDf = oneMinDf[oneMinIndex:]

    # 继续执行主程序，使用timestamp
    except Exception as e:
        print(e,"日期或時間格式無效,從資料最開始進行回放")
        pass

    window.destroy()
    # 继续执行主程序
    asyncio.run(main())


if __name__ == '__main__':
# 创建主窗口
    window = tk.Tk()
    window.title("start program")

    label = tk.Label(window, text="請選擇日期：")
    label.pack(pady=10)

    calendar = Calendar(window, date_pattern="yyyy-MM-dd")
    calendar.pack()

    minTimeStr = minTime.strftime("%Y-%m-%d %H:%M")
    maxTimeStr = maxTime.strftime("%Y-%m-%d %H:%M")

    label_time = tk.Label(window, text=f"請輸入時間（24小時制，HH:MM）,最小時間:{minTimeStr},最大時間:{maxTimeStr}")
    label_time.pack()

    # 创建一个框架来放置时间输入
    time_frame = tk.Frame(window)
    time_frame.pack()

    hour_var = tk.StringVar()
    hour_entry = tk.Entry(time_frame, textvariable=hour_var, width=2)
    hour_entry.grid(row=0, column=0) # 使用grid定位

    colon_label = tk.Label(time_frame, text=":")
    colon_label.grid(row=0, column=1) # 使用grid定位

    minute_var = tk.StringVar()
    minute_entry = tk.Entry(time_frame, textvariable=minute_var, width=2)
    minute_entry.grid(row=0, column=2) # 使用grid定位

    # 创建一个框架来放置按钮
    button_frame = tk.Frame(window)
    button_frame.pack()

    start_button = tk.Button(button_frame, text="开始", command=start_program)
    start_button.grid(row=0, column=0) # 使用grid定位

    cancel_button = tk.Button(button_frame, text="取消", command=window.destroy)
    cancel_button.grid(row=0, column=1) # 使用grid定位

    window.mainloop()