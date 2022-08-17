import { StyleSheet } from 'react-native';

import { SEMIBOLD } from '@/constants/fonts';
import { Colors } from '@/constants/theme';
import { fontMaker } from '@/utils/fonts';

const commonBorderRadius = 16;
export const defaultPlaceholderTextColor = Colors.White.Secondary;
export const errorColor = Colors.Red;

export const getCustomGradient = (color: string) => ({
  start: {
    x: 0,
    y: 0,
  },
  end: {
    x: 1,
    y: 1,
  },
  colors: [color, color],
});

const styles = StyleSheet.create({
  textInput: {
    ...fontMaker({ size: 18, color: Colors.White.Pure, weight: SEMIBOLD }),
    borderRadius: commonBorderRadius,
    flexGrow: 1,
  },
  singleLine: {
    height: 56,
  },
  innerContainer: {
    backgroundColor: Colors.Black.Primary,
    alignItems: 'center',
    flexDirection: 'row',
    borderRadius: commonBorderRadius,
    paddingHorizontal: 16,
    flexGrow: 1,
  },
  multilineContainer: {
    alignItems: 'flex-start',
  },
  gradientContainer: {
    borderRadius: commonBorderRadius,
    alignItems: 'center',
    justifyContent: 'center',
    padding: 1,
  },
});

export default styles;
