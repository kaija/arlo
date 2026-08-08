import React from 'react';
import { Icon } from '../icons/Icon.jsx';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Tag({ tone, onRemove, onClick, children, ...rest }) {
  const Tag_ = onClick ? 'button' : 'span';
  return (
    <Tag_ className={cx('tag', tone === 'accent' && 'tag-accent', onClick && 'tag-button')} onClick={onClick} {...rest}>
      {children}
      {onRemove && (
        <button type="button" className="tag-remove" aria-label="Remove" onClick={e => { e.stopPropagation(); onRemove(e); }}>
          <Icon name="close" size={13} strokeWidth={2.25} />
        </button>
      )}
    </Tag_>
  );
}
