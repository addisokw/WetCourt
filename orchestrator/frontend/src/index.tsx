/* @refresh reload */
import { render } from 'solid-js/web';
import Shell from './Shell';
import FaceView from './FaceView';
import CaseView from './CaseView';
import './robotSettings'; // seed the robot-TTS graph from localStorage at startup
import './app.css';

const path = location.pathname.replace(/\/+$/, '');

function Root() {
  if (path === '/face') return <FaceView />;
  if (path === '/case') return <CaseView />;
  return <Shell />;
}

render(() => <Root />, document.getElementById('root')!);
