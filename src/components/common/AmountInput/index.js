import React, { useRef, useState } from 'react';
import { Text, TextInput, View } from 'react-native';
import LinearGradient from 'react-native-linear-gradient';

import animationScales from '../../../utils/animationScales';
import { Rainbow, Colors } from '../../../constants/theme';
import Touchable from '../../animations/Touchable';
import Button from '../../buttons/Button';
import styles from './styles';

const AmountInput = ({
  value,
  onChange,
  selected,
  setSelected,
  symbol,
  maxAmount,
  autoFocus,
  customStyle,
}) => {
  const inputRef = useRef();
  const [isFocused, setIsFocused] = useState(false);

  const handleOnFocus = () => {
    setIsFocused(true);
    setSelected(symbol);
  };

  const handleOnBlur = () => {
    setIsFocused(false);
  };

  const handleMaxAmount = () => {
    onChange(String(maxAmount));
  };

  const onPress = () => {
    inputRef?.current.focus();
    setSelected(symbol);
  };

  return (
    <Touchable scale={animationScales.small} onPress={onPress}>
      {isFocused && (
        <LinearGradient
          style={[styles.focusedGradient, customStyle]}
          {...Rainbow}
        />
      )}
      <View style={[styles.container, customStyle]}>
        <TextInput
          ref={inputRef}
          underlineColorAndroid="transparent"
          style={styles.textInput}
          placeholderTextColor="#373946"
          onChangeText={onChange}
          value={value}
          keyboardType="numeric"
          placeholder="0.0"
          blurOnSubmit={false}
          autoFocus={autoFocus}
          keyboardAppearance="dark"
          selectionColor={Colors.White.Primary}
          onFocus={handleOnFocus}
          onBlur={handleOnBlur}
        />
        {selected && maxAmount && (
          <Button
            variant="gray"
            text="Max"
            onPress={() => handleMaxAmount()}
            buttonStyle={styles.buttonStyle}
            textStyle={styles.buttonTextStyle}
          />
        )}
        <Text style={styles.symbol}>{symbol}</Text>
      </View>
    </Touchable>
  );
};

export default AmountInput;
