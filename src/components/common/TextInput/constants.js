import styles from './styles';
import { Colors } from '../../../constants/theme';

export const variants = {
  password: {
    viewStyle: styles.viewStyle,
    inputStyle: styles.inputStyle,
    placeholderTextColor: Colors.White.Secondary,
    autoCorrect: false,
    autoCapitalize: 'none',
    secureTextEntry: true,
  },
  innerLabel: {
    viewStyle: { ...styles.viewStyle, ...styles.labledViewStyle },
    inputStyle: { ...styles.inputStyle, ...styles.labledInputStyle },
    innerLabelStyle: styles.innerLabelStyle,
    placeholderTextColor: Colors.White.Secondary,
    autoCorrect: false,
    autoCapitalize: 'none',
    secureTextEntry: false,
  },
};